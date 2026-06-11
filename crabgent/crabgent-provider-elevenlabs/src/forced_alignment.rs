//! `ElevenLabs` forced-alignment transport.

use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use crabgent_core::{
    ForcedAlignedCharacter, ForcedAlignedWord, ForcedAlignmentError, ForcedAlignmentProvider,
    ForcedAlignmentProviderCapabilities, ForcedAlignmentRequest, ForcedAlignmentResponse,
};
use crabgent_log::warn;
use crabgent_provider_transport::read_text_body;
use reqwest::multipart::{Form, Part};
use secrecy::ExposeSecret;
use serde::Deserialize;

use crate::tts::ElevenLabsTtsProvider;

const FORCED_ALIGNMENT_ENDPOINT: &str = "/v1/forced-alignment";
const ALIGNMENT_ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ALIGNMENT_ERROR_BODY_BYTES: usize = 64 * 1024;

#[async_trait]
impl ForcedAlignmentProvider for ElevenLabsTtsProvider {
    async fn align(
        &self,
        req: ForcedAlignmentRequest,
    ) -> Result<ForcedAlignmentResponse, ForcedAlignmentError> {
        let form = build_form(&req)?;
        let endpoint = endpoint_url(self.config().api_base());
        let response = self
            .http_client()
            .post(&endpoint)
            .header("xi-api-key", self.config().api_key.expose_secret())
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                warn!(%endpoint, %error, "elevenlabs forced-alignment send failed");
                ForcedAlignmentError::Network
            })?;

        let status = response.status();
        if status.is_success() {
            let raw = response
                .json::<RawAlignmentResponse>()
                .await
                .map_err(|err| {
                    warn!(error = %err, "elevenlabs forced-alignment decode failed");
                    ForcedAlignmentError::Decode
                })?;
            return Ok(raw.into_response());
        }

        if matches!(status.as_u16(), 401 | 403) {
            return Err(ForcedAlignmentError::Auth(
                "elevenlabs authentication failed".to_owned(),
            ));
        }

        let body_len = read_text_body(
            response,
            None,
            ALIGNMENT_ERROR_BODY_TIMEOUT,
            MAX_ALIGNMENT_ERROR_BODY_BYTES,
        )
        .await
        .map(|body| body.len())
        .unwrap_or_default();
        warn!(status = %status, body_len, "elevenlabs forced-alignment request failed");

        Err(ForcedAlignmentError::Backend(
            "elevenlabs forced-alignment request failed".to_owned(),
        ))
    }

    fn forced_alignment_capabilities(&self) -> ForcedAlignmentProviderCapabilities {
        ForcedAlignmentProviderCapabilities {
            character_timing: true,
            word_timing: true,
        }
    }
}

fn build_form(req: &ForcedAlignmentRequest) -> Result<Form, ForcedAlignmentError> {
    let filename = req
        .payload
        .filename
        .clone()
        .unwrap_or_else(|| "audio".to_owned());
    let bytes = Bytes::copy_from_slice(req.payload.bytes().as_ref());
    let file_part = Part::stream(reqwest::Body::from(bytes))
        .file_name(filename)
        .mime_str(req.payload.mime())
        .map_err(|err| {
            warn!(error = %err, "elevenlabs forced-alignment multipart mime rejected");
            ForcedAlignmentError::Decode
        })?;

    Ok(Form::new()
        .part("file", file_part)
        .text("text", req.text.clone()))
}

fn endpoint_url(base_url: &str) -> String {
    format!(
        "{}{}",
        base_url.trim_end_matches('/'),
        FORCED_ALIGNMENT_ENDPOINT
    )
}

#[derive(Deserialize)]
struct RawAlignmentResponse {
    #[serde(default)]
    characters: Vec<RawAlignedCharacter>,
    #[serde(default)]
    words: Vec<RawAlignedWord>,
    loss: Option<f32>,
}

impl RawAlignmentResponse {
    fn into_response(self) -> ForcedAlignmentResponse {
        ForcedAlignmentResponse {
            characters: self
                .characters
                .into_iter()
                .map(RawAlignedCharacter::into_character)
                .collect(),
            words: self
                .words
                .into_iter()
                .map(RawAlignedWord::into_word)
                .collect(),
            loss: self.loss,
        }
    }
}

#[derive(Deserialize)]
struct RawAlignedCharacter {
    text: String,
    start: f32,
    end: f32,
}

impl RawAlignedCharacter {
    fn into_character(self) -> ForcedAlignedCharacter {
        ForcedAlignedCharacter {
            text: self.text,
            start: self.start,
            end: self.end,
        }
    }
}

#[derive(Deserialize)]
struct RawAlignedWord {
    text: String,
    start: f32,
    end: f32,
    loss: Option<f32>,
}

impl RawAlignedWord {
    fn into_word(self) -> ForcedAlignedWord {
        ForcedAlignedWord {
            text: self.text,
            start: self.start,
            end: self.end,
            loss: self.loss,
        }
    }
}

#[cfg(test)]
mod tests {
    use crabgent_core::AudioPayload;

    use super::*;
    use crate::ElevenLabsConfig;

    const XI_API_KEY: &str = "secret-test-xi-key-99999";

    fn provider_for(base: &str) -> ElevenLabsTtsProvider {
        ElevenLabsTtsProvider::new(ElevenLabsConfig::new(XI_API_KEY).with_api_base(base))
    }

    fn request() -> ForcedAlignmentRequest {
        ForcedAlignmentRequest {
            payload: AudioPayload::new(
                b"RIFFfake".to_vec(),
                "audio/wav",
                Some("speech_sample.wav".to_owned()),
            )
            .expect("valid audio payload"),
            text: "Hello world".to_owned(),
        }
    }

    #[tokio::test]
    async fn forced_alignment_success_returns_character_and_word_timings() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/forced-alignment")
            .match_header("xi-api-key", XI_API_KEY)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "characters": [
                        {"text": "H", "start": 0.0, "end": 0.05},
                        {"text": "i", "start": 0.05, "end": 0.1}
                    ],
                    "words": [
                        {"text": "Hi", "start": 0.0, "end": 0.1, "loss": 0.02}
                    ],
                    "loss": 0.03
                }"#,
            )
            .expect(1)
            .create_async()
            .await;

        let response = provider_for(&server.url())
            .align(request())
            .await
            .expect("align ok");

        assert_eq!(response.characters.len(), 2);
        assert_eq!(response.characters[0].text, "H");
        assert_eq!(response.words.len(), 1);
        assert_eq!(response.words[0].text, "Hi");
        assert_eq!(response.words[0].loss, Some(0.02));
        assert_eq!(response.loss, Some(0.03));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn auth_failure_returns_opaque_error_no_key_leak() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/forced-alignment")
            .with_status(401)
            .with_body("invalid xi-api-key")
            .expect(1)
            .create_async()
            .await;

        let error = provider_for(&server.url())
            .align(request())
            .await
            .expect_err("auth failure");

        assert!(matches!(error, ForcedAlignmentError::Auth(_)));
        let rendered = format!("{error} {error:?}");
        assert!(
            !rendered.contains(XI_API_KEY),
            "error string must not leak the api key"
        );
        assert!(
            !rendered.contains("invalid xi-api-key"),
            "error string must not leak the response body"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn backend_failure_is_opaque() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/forced-alignment")
            .with_status(422)
            .with_body("text does not match audio")
            .expect(1)
            .create_async()
            .await;

        let error = provider_for(&server.url())
            .align(request())
            .await
            .expect_err("backend failure");

        assert!(matches!(error, ForcedAlignmentError::Backend(_)));
        let rendered = format!("{error} {error:?}");
        assert!(
            !rendered.contains("text does not match audio"),
            "error string must not leak the response body"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn decode_failure_maps_to_decode() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/forced-alignment")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{not-json")
            .expect(1)
            .create_async()
            .await;

        let error = provider_for(&server.url())
            .align(request())
            .await
            .expect_err("decode failure");

        assert!(matches!(error, ForcedAlignmentError::Decode));
        mock.assert_async().await;
    }

    #[test]
    fn endpoint_url_trims_trailing_slash() {
        assert_eq!(
            endpoint_url("https://api.elevenlabs.io/"),
            "https://api.elevenlabs.io/v1/forced-alignment"
        );
    }

    #[test]
    fn capabilities_advertise_character_and_word_timing() {
        let capabilities =
            provider_for("https://api.elevenlabs.io").forced_alignment_capabilities();
        assert!(capabilities.character_timing);
        assert!(capabilities.word_timing);
    }
}
