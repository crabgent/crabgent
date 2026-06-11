//! `OpenAI` text-to-speech transport.
//!
//! `/v1/audio/speech` is a plain JSON POST that returns raw audio bytes.
//! Unlike STT, there is no WebSocket path, so this provider holds only the
//! HTTP client, config, and auth strategy.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{TtsError, TtsProvider, TtsProviderCapabilities, TtsRequest, TtsResponse};
use crabgent_log::warn;
use reqwest::StatusCode;
use serde_json::json;

use crate::auth::AuthStrategy;
use crate::types::{OpenAiConfig, OpenAiError};

const SPEECH_ENDPOINT: &str = "/v1/audio/speech";

/// `OpenAI` text-to-speech provider.
pub struct OpenAiTtsProvider {
    config: Arc<OpenAiConfig>,
    auth: Arc<dyn AuthStrategy>,
    http: reqwest::Client,
}

impl OpenAiTtsProvider {
    /// Build a provider from an HTTP client, config, and auth strategy.
    ///
    /// Mirrors the STT constructor: the config is validated up front and a
    /// fail-closed [`OpenAiError::ConfigError`] is returned for an empty key
    /// or a zero request timeout.
    pub fn new(
        http: reqwest::Client,
        config: Arc<OpenAiConfig>,
        auth: Arc<dyn AuthStrategy>,
    ) -> Result<Self, OpenAiError> {
        crate::stt::validate_config(&config)?;
        Ok(Self { config, auth, http })
    }

    #[must_use]
    pub const fn config(&self) -> &Arc<OpenAiConfig> {
        &self.config
    }

    #[must_use]
    pub fn auth(&self) -> &dyn AuthStrategy {
        self.auth.as_ref()
    }

    #[must_use]
    pub const fn http_client(&self) -> &reqwest::Client {
        &self.http
    }
}

fn authenticated_builder(
    mut builder: reqwest::RequestBuilder,
    auth: &dyn AuthStrategy,
) -> reqwest::RequestBuilder {
    for (name, value) in auth.auth_headers() {
        builder = builder.header(name, value);
    }
    builder
}

fn speech_url(base_url: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), SPEECH_ENDPOINT)
}

/// Map a non-success HTTP status onto an opaque [`TtsError`]. Never forwards
/// the response body or any secret: only the numeric status escapes.
fn map_status_error(status: StatusCode) -> TtsError {
    warn!(status = %status, "openai tts request failed");
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return TtsError::Auth("openai tts authentication failed".to_owned());
    }
    TtsError::Backend(format!("openai tts request failed: status={status}"))
}

#[async_trait]
impl TtsProvider for OpenAiTtsProvider {
    async fn synthesize(&self, req: TtsRequest) -> Result<TtsResponse, TtsError> {
        // All six `TtsAudioFormat` variants map onto a `response_format` value
        // OpenAI accepts, so `FormatUnsupported` is never returned here.
        let url = speech_url(self.auth.base_url());
        let body = json!({
            "model": req.model.as_str(),
            "input": req.text,
            "voice": req.voice.as_str(),
            "response_format": req.format.as_neutral_str(),
        });
        let builder = authenticated_builder(self.http.post(&url).json(&body), self.auth.as_ref());
        let response = builder.send().await.map_err(|err| {
            warn!(error = %err, "openai tts request send failed");
            TtsError::Network
        })?;

        let status = response.status();
        if !status.is_success() {
            // map_status_error logs the status; the body is never read here.
            return Err(map_status_error(status));
        }

        let bytes = response.bytes().await.map_err(|_err| TtsError::Decode)?;
        Ok(TtsResponse {
            audio: Arc::from(bytes.as_ref()),
            mime: req.format.mime().to_owned(),
            model: req.model,
        })
    }

    fn capabilities(&self) -> TtsProviderCapabilities {
        TtsProviderCapabilities { streaming: false }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crabgent_core::{TtsAudioFormat, TtsModelId, VoiceId};
    use mockito::Matcher;
    use secrecy::SecretString;

    use super::*;
    use crate::auth::ApiKeyAuth;

    const API_KEY_SECRET: &str = "secret-test-key-99999";
    const AUDIO_BYTES: &[u8] = &[0x49, 0x44, 0x33, 0x04, 0x00, 0xff];

    fn config() -> Arc<OpenAiConfig> {
        Arc::new(
            OpenAiConfig::new(API_KEY_SECRET)
                .with_max_retries(0)
                .with_request_timeout(Duration::from_secs(2)),
        )
    }

    fn provider(base_url: &str) -> OpenAiTtsProvider {
        let auth = ApiKeyAuth::new(SecretString::from(API_KEY_SECRET.to_owned()))
            .with_base_url(base_url.to_owned());
        OpenAiTtsProvider::new(reqwest::Client::new(), config(), Arc::new(auth))
            .expect("valid tts provider config")
    }

    fn request(format: TtsAudioFormat) -> TtsRequest {
        TtsRequest {
            text: "hello world".to_owned(),
            model: TtsModelId::new("gpt-4o-mini-tts"),
            voice: VoiceId::new("coral"),
            format,
        }
    }

    #[test]
    fn speech_url_trims_trailing_slash() {
        assert_eq!(
            speech_url("https://api.openai.com/"),
            "https://api.openai.com/v1/audio/speech"
        );
    }

    #[tokio::test]
    async fn synthesize_success_returns_bytes_and_mime() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/audio/speech")
            .match_header(
                "authorization",
                Matcher::Exact(format!("Bearer {API_KEY_SECRET}")),
            )
            .with_status(200)
            .with_header("content-type", "audio/mpeg")
            .with_body(AUDIO_BYTES)
            .create_async()
            .await;

        let response = provider(&server.url())
            .synthesize(request(TtsAudioFormat::Mp3))
            .await
            .expect("tts response");

        mock.assert_async().await;
        assert_eq!(response.audio.as_ref(), AUDIO_BYTES);
        assert_eq!(response.mime, "audio/mpeg");
        assert_eq!(response.model.as_str(), "gpt-4o-mini-tts");
    }

    #[tokio::test]
    async fn auth_failure_opaque_no_key_leak() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/v1/audio/speech")
            .with_status(401)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":{"message":"Invalid API key: secret-test-key-99999"}}"#)
            .create_async()
            .await;

        let err = provider(&server.url())
            .synthesize(request(TtsAudioFormat::Mp3))
            .await
            .expect_err("auth failure");

        match &err {
            TtsError::Auth(_) => {}
            other => panic!("expected TtsError::Auth, got {other:?}"),
        }
        let rendered = err.to_string();
        assert!(
            !rendered.contains(API_KEY_SECRET),
            "error string leaked the api key"
        );
        assert!(
            !rendered.contains("Invalid API key"),
            "error string leaked the response body"
        );
    }

    #[tokio::test]
    async fn request_body_carries_neutral_fields() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/audio/speech")
            .match_body(Matcher::AllOf(vec![
                Matcher::PartialJson(serde_json::json!({
                    "voice": "coral",
                    "response_format": "wav",
                })),
                Matcher::Regex(r#""input""#.to_owned()),
            ]))
            .with_status(200)
            .with_header("content-type", "audio/wav")
            .with_body(AUDIO_BYTES)
            .create_async()
            .await;

        let response = provider(&server.url())
            .synthesize(request(TtsAudioFormat::Wav))
            .await
            .expect("tts response");

        mock.assert_async().await;
        assert_eq!(response.mime, "audio/wav");
    }
}
