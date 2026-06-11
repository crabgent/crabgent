//! `ElevenLabs` text-to-speech provider implementation.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{
    TtsAudioFormat, TtsError, TtsProvider, TtsProviderCapabilities, TtsRequest, TtsResponse,
};
use crabgent_log::warn;
use secrecy::ExposeSecret;
use serde::Serialize;

use crate::config::ElevenLabsConfig;

/// `ElevenLabs` text-to-speech provider.
pub struct ElevenLabsTtsProvider {
    config: Arc<ElevenLabsConfig>,
    http: reqwest::Client,
    voice_settings: Option<ElevenLabsVoiceSettings>,
}

/// Per-request `ElevenLabs` voice settings applied to synthesized speech.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ElevenLabsVoiceSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stability: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity_boost: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_speaker_boost: Option<bool>,
}

impl ElevenLabsVoiceSettings {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.stability.is_none()
            && self.similarity_boost.is_none()
            && self.style.is_none()
            && self.speed.is_none()
            && self.use_speaker_boost.is_none()
    }
}

impl ElevenLabsTtsProvider {
    /// Build a provider from config, mirroring the STT provider's reqwest
    /// client construction.
    #[must_use]
    pub fn new(config: ElevenLabsConfig) -> Self {
        Self {
            config: Arc::new(config),
            http: crabgent_provider_transport::hardened_client(),
            voice_settings: None,
        }
    }

    /// Apply ElevenLabs-specific voice settings to every synthesized request.
    #[must_use]
    pub const fn voice_settings(mut self, settings: ElevenLabsVoiceSettings) -> Self {
        self.voice_settings = Some(settings);
        self
    }

    pub(crate) fn config(&self) -> &ElevenLabsConfig {
        &self.config
    }

    pub(crate) const fn http_client(&self) -> &reqwest::Client {
        &self.http
    }
}

#[derive(Serialize)]
struct ElevenLabsTtsRequestBody<'a> {
    text: &'a str,
    model_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice_settings: Option<&'a ElevenLabsVoiceSettings>,
}

impl<'a> ElevenLabsTtsRequestBody<'a> {
    fn new(req: &'a TtsRequest, settings: Option<&'a ElevenLabsVoiceSettings>) -> Self {
        Self {
            text: &req.text,
            model_id: req.model.as_str(),
            voice_settings: settings.filter(|settings| !settings.is_empty()),
        }
    }
}

/// Map the neutral output format to the `ElevenLabs` `output_format` query value.
///
/// `ElevenLabs` has no AAC/FLAC output, so those collapse to
/// [`TtsError::FormatUnsupported`] before any HTTP call.
const fn output_format(format: TtsAudioFormat) -> Result<&'static str, TtsError> {
    match format {
        TtsAudioFormat::Mp3 => Ok("mp3_44100_128"),
        TtsAudioFormat::Opus => Ok("opus_48000_128"),
        TtsAudioFormat::Pcm => Ok("pcm_24000"),
        TtsAudioFormat::Wav => Ok("wav_44100"),
        TtsAudioFormat::Aac | TtsAudioFormat::Flac => Err(TtsError::FormatUnsupported),
    }
}

/// Build the request URL without string-formatting the untrusted voice id into
/// the path. `path_segments_mut` percent-encodes each segment and prevents
/// `..` traversal.
fn synthesize_url(api_base: &str, voice: &str, mapped: &str) -> Result<reqwest::Url, TtsError> {
    let base = api_base.trim_end_matches('/');
    let mut url = reqwest::Url::parse(base)
        .map_err(|_parse| TtsError::Backend("invalid elevenlabs base url".to_owned()))?;
    url.path_segments_mut()
        .map_err(|()| TtsError::Backend("invalid elevenlabs base url".to_owned()))?
        .extend(["v1", "text-to-speech", voice]);
    url.set_query(Some(&format!("output_format={mapped}")));
    Ok(url)
}

#[async_trait]
impl TtsProvider for ElevenLabsTtsProvider {
    async fn synthesize(&self, req: TtsRequest) -> Result<TtsResponse, TtsError> {
        let mapped = output_format(req.format)?;
        let url = synthesize_url(self.config.api_base(), req.voice.as_str(), mapped)?;
        let body = ElevenLabsTtsRequestBody::new(&req, self.voice_settings.as_ref());

        let response = self
            .http
            .post(url)
            .header("xi-api-key", self.config.api_key.expose_secret())
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                warn!(%error, "elevenlabs text-to-speech send failed");
                TtsError::Network
            })?;

        let status = response.status();
        if status.is_success() {
            let bytes = response.bytes().await.map_err(|_read| TtsError::Decode)?;
            return Ok(TtsResponse {
                audio: Arc::from(bytes.as_ref()),
                mime: req.format.mime().to_owned(),
                model: req.model,
            });
        }

        if matches!(status.as_u16(), 401 | 403) {
            return Err(TtsError::Auth(
                "elevenlabs authentication failed".to_owned(),
            ));
        }

        Err(TtsError::Backend(
            "elevenlabs text-to-speech request failed".to_owned(),
        ))
    }

    fn capabilities(&self) -> TtsProviderCapabilities {
        TtsProviderCapabilities { streaming: false }
    }
}

#[cfg(test)]
mod tests {
    use crabgent_core::{TtsModelId, VoiceId};

    use super::*;

    const XI_API_KEY: &str = "secret-test-xi-key-99999";

    fn tts_request(format: TtsAudioFormat) -> TtsRequest {
        TtsRequest {
            text: "hello world".to_owned(),
            model: TtsModelId::new("eleven_multilingual_v2"),
            voice: VoiceId::new("voice-abc"),
            format,
        }
    }

    fn provider_for(base: &str) -> ElevenLabsTtsProvider {
        ElevenLabsTtsProvider::new(ElevenLabsConfig::new(XI_API_KEY).with_api_base(base))
    }

    #[tokio::test]
    async fn synthesize_success_returns_bytes_and_mime() {
        let mut server = mockito::Server::new_async().await;
        let audio = b"\x49\x44\x33raw-mp3-bytes".to_vec();
        let mock = server
            .mock("POST", "/v1/text-to-speech/voice-abc")
            .match_query(mockito::Matcher::UrlEncoded(
                "output_format".to_owned(),
                "mp3_44100_128".to_owned(),
            ))
            .match_header("xi-api-key", XI_API_KEY)
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "text": "hello world",
                "model_id": "eleven_multilingual_v2"
            })))
            .with_status(200)
            .with_body(audio.clone())
            .expect(1)
            .create_async()
            .await;

        let response = provider_for(&server.url())
            .synthesize(tts_request(TtsAudioFormat::Mp3))
            .await
            .expect("synthesize ok");

        assert_eq!(response.audio.as_ref(), audio.as_slice());
        assert_eq!(response.mime, "audio/mpeg");
        assert_eq!(response.model.as_str(), "eleven_multilingual_v2");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn synthesize_sends_configured_voice_settings() {
        let mut server = mockito::Server::new_async().await;
        let audio = b"\x49\x44\x33raw-mp3-bytes".to_vec();
        let mock = server
            .mock("POST", "/v1/text-to-speech/voice-abc")
            .match_query(mockito::Matcher::Any)
            .match_header("xi-api-key", XI_API_KEY)
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "text": "hello world",
                "model_id": "eleven_multilingual_v2",
                "voice_settings": {
                    "stability": 0.35,
                    "similarity_boost": 0.75,
                    "style": 0.55,
                    "speed": 1.0,
                    "use_speaker_boost": true
                }
            })))
            .with_status(200)
            .with_body(audio)
            .expect(1)
            .create_async()
            .await;

        let settings = ElevenLabsVoiceSettings {
            stability: Some(0.35),
            similarity_boost: Some(0.75),
            style: Some(0.55),
            speed: Some(1.0),
            use_speaker_boost: Some(true),
        };
        provider_for(&server.url())
            .voice_settings(settings)
            .synthesize(tts_request(TtsAudioFormat::Mp3))
            .await
            .expect("synthesize ok");

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn auth_failure_returns_opaque_auth_error_no_key_leak() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/text-to-speech/voice-abc")
            .match_query(mockito::Matcher::Any)
            .with_status(401)
            .with_body("invalid xi-api-key")
            .expect(1)
            .create_async()
            .await;

        let error = provider_for(&server.url())
            .synthesize(tts_request(TtsAudioFormat::Mp3))
            .await
            .expect_err("auth failure");

        assert!(matches!(error, TtsError::Auth(_)));
        let rendered = format!("{error} {error:?}");
        assert!(
            !rendered.contains(XI_API_KEY),
            "error string must not leak the api key"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn aac_format_unsupported_without_http_call() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;

        let error = provider_for(&server.url())
            .synthesize(tts_request(TtsAudioFormat::Aac))
            .await
            .expect_err("aac unsupported");

        assert!(matches!(error, TtsError::FormatUnsupported));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn flac_format_unsupported_without_http_call() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;

        let error = provider_for(&server.url())
            .synthesize(tts_request(TtsAudioFormat::Flac))
            .await
            .expect_err("flac unsupported");

        assert!(matches!(error, TtsError::FormatUnsupported));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn backend_error_on_non_auth_failure_status() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/text-to-speech/voice-abc")
            .match_query(mockito::Matcher::Any)
            .with_status(500)
            .with_body("upstream boom")
            .expect(1)
            .create_async()
            .await;

        let error = provider_for(&server.url())
            .synthesize(tts_request(TtsAudioFormat::Mp3))
            .await
            .expect_err("backend failure");

        assert!(matches!(error, TtsError::Backend(_)));
        let rendered = format!("{error} {error:?}");
        assert!(
            !rendered.contains("upstream boom"),
            "error string must not leak the response body"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn capabilities_streaming_is_false() {
        let provider = provider_for("https://api.elevenlabs.io");
        assert!(!provider.capabilities().streaming);
    }

    #[test]
    fn opus_pcm_wav_map_to_elevenlabs_format() {
        assert_eq!(
            output_format(TtsAudioFormat::Opus).expect("opus"),
            "opus_48000_128"
        );
        assert_eq!(
            output_format(TtsAudioFormat::Pcm).expect("pcm"),
            "pcm_24000"
        );
        assert_eq!(
            output_format(TtsAudioFormat::Wav).expect("wav"),
            "wav_44100"
        );
    }

    #[test]
    fn synthesize_url_percent_encodes_voice_and_blocks_traversal() {
        let url = synthesize_url("https://api.elevenlabs.io", "../../etc/x", "mp3_44100_128")
            .expect("url builds");
        // The slashes inside the voice id are percent-encoded, so the segment
        // stays a single path segment and cannot escape /v1/text-to-speech/.
        assert!(
            url.path().contains("%2F"),
            "voice id slashes must be percent-encoded"
        );
        assert!(
            !url.path().contains("/etc/"),
            "voice id must not escape into a new raw path segment"
        );
        assert!(url.path().starts_with("/v1/text-to-speech/"));
        assert_eq!(
            url.path_segments().expect("cannot-be-base").count(),
            3,
            "voice id stays exactly one trailing segment"
        );
        assert_eq!(url.query(), Some("output_format=mp3_44100_128"));
    }

    #[test]
    fn synthesize_url_rejects_unparseable_base() {
        let error = synthesize_url("not a url", "voice", "mp3_44100_128").expect_err("bad base");
        assert!(matches!(error, TtsError::Backend(_)));
    }
}
