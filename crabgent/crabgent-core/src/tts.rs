//! Text-to-speech provider surface.
//!
//! Parallel to [`crate::stt`] and [`crate::image_generation`]: a net-new
//! provider trait, not part of the LLM [`crate::provider::Provider`] surface.
//! Like [`crate::image_generation::ImageGenerationProvider`] there is no
//! `stream` method. The only consumer (the `speak` tool) buffers the full
//! audio before storing it, so a streaming variant has no consumer yet and is
//! added when one exists (Spirit: no layer without a consumer).

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::newtype::string_newtype;

/// Abstraction over text-to-speech backends.
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Synthesize speech audio from one request.
    async fn synthesize(&self, req: TtsRequest) -> Result<TtsResponse, TtsError>;

    /// Provider-wide capability advertisement.
    fn capabilities(&self) -> TtsProviderCapabilities;

    /// Models this TTS provider can serve.
    fn models(&self) -> Vec<TtsModelInfo> {
        Vec::new()
    }

    /// Fetch the provider's current TTS model list.
    async fn fetch_models(&self) -> Result<Vec<TtsModelInfo>, TtsError> {
        Ok(self.models())
    }
}

/// Request passed to a TTS provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TtsRequest {
    pub text: String,
    pub model: TtsModelId,
    pub voice: VoiceId,
    pub format: TtsAudioFormat,
}

/// Synthesized speech audio.
///
/// `audio` holds the raw encoded bytes in `mime` format. They are
/// provider-generated and already validated at the source; consumers store
/// them verbatim and must not re-validate across the trust boundary.
#[derive(Clone, PartialEq, Eq)]
pub struct TtsResponse {
    pub audio: Arc<[u8]>,
    pub mime: String,
    pub model: TtsModelId,
}

impl fmt::Debug for TtsResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Render the byte length, never the audio payload itself.
        f.debug_struct("TtsResponse")
            .field("audio_len", &self.audio.len())
            .field("mime", &self.mime)
            .field("model", &self.model)
            .finish()
    }
}

/// Errors returned by TTS providers.
///
/// `Auth` and `Backend` carry a caller-supplied opaque description.
/// Providers must keep these free of secrets and raw response bodies.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum TtsError {
    #[error("auth error: {0}")]
    Auth(String),
    #[error("network error")]
    Network,
    #[error("backend error: {0}")]
    Backend(String),
    #[error("decode error")]
    Decode,
    #[error("model discovery failed: {reason}")]
    ModelDiscovery { reason: String },
    #[error("unknown TTS model")]
    ModelUnknown,
    #[error("audio format not supported by provider")]
    FormatUnsupported,
    #[error("input too long: {len} chars exceeds max {max}")]
    InputTooLong { len: usize, max: usize },
}

/// Provider-neutral output audio format.
///
/// Each provider maps these to its own format identifier and returns
/// [`TtsError::FormatUnsupported`] for formats it cannot emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsAudioFormat {
    #[default]
    Mp3,
    Opus,
    Aac,
    Flac,
    Wav,
    Pcm,
}

impl TtsAudioFormat {
    /// Neutral lowercase identifier (matches the serde wire form).
    #[must_use]
    pub const fn as_neutral_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Opus => "opus",
            Self::Aac => "aac",
            Self::Flac => "flac",
            Self::Wav => "wav",
            Self::Pcm => "pcm",
        }
    }

    /// Canonical MIME type for the encoded audio in this format.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Opus => "audio/ogg",
            Self::Aac => "audio/aac",
            Self::Flac => "audio/flac",
            Self::Wav => "audio/wav",
            Self::Pcm => "audio/L16",
        }
    }
}

/// Stable identifier of a TTS model.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TtsModelId(String);

string_newtype!(trim TtsModelId);

/// Stable identifier of a synthesis voice.
///
/// Opaque, provider-interpreted: `ElevenLabs` uses a voice id in the URL path,
/// `OpenAI` uses a named voice. The neutral request carries the string verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VoiceId(String);

string_newtype!(trim VoiceId);

/// Metadata for one TTS model.
///
/// Intentionally minimal (`id` only): there is no streaming or voice-discovery
/// consumer for TTS yet. Fields are added when a consumer needs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TtsModelInfo {
    pub id: TtsModelId,
}

/// Provider-wide TTS capabilities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TtsProviderCapabilities {
    pub streaming: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tts_model_id_trims_whitespace() {
        let id = TtsModelId::new(" eleven_multilingual_v2 ");
        assert_eq!(id.as_str(), "eleven_multilingual_v2");
        assert_eq!(id.to_string(), "eleven_multilingual_v2");
    }

    #[test]
    fn tts_model_id_from_variants_agree() {
        let owned = String::from("tts-1");
        assert_eq!(TtsModelId::from("tts-1"), TtsModelId::from(owned.clone()));
        assert_eq!(TtsModelId::from(&owned), TtsModelId::from("tts-1"));
    }

    #[test]
    fn voice_id_trims_and_displays() {
        let voice = VoiceId::new(" alloy ");
        assert_eq!(voice.as_str(), "alloy");
        assert_eq!(voice.to_string(), "alloy");
        assert_eq!(VoiceId::from("alloy"), voice);
        let owned = String::from("alloy");
        assert_eq!(VoiceId::from(&owned), voice);
    }

    #[test]
    fn ids_serde_are_transparent() {
        let model = TtsModelId::new("gpt-4o-mini-tts");
        assert_eq!(
            serde_json::to_value(&model).expect("ser"),
            serde_json::json!("gpt-4o-mini-tts")
        );
        let back: TtsModelId =
            serde_json::from_value(serde_json::json!("gpt-4o-mini-tts")).expect("de");
        assert_eq!(back, model);

        let voice = VoiceId::new("nova");
        assert_eq!(
            serde_json::to_value(&voice).expect("ser"),
            serde_json::json!("nova")
        );
        let back: VoiceId = serde_json::from_value(serde_json::json!("nova")).expect("de");
        assert_eq!(back, voice);
    }

    #[test]
    fn audio_format_default_is_mp3() {
        assert_eq!(TtsAudioFormat::default(), TtsAudioFormat::Mp3);
    }

    #[test]
    fn audio_format_serde_is_snake_case() {
        for (fmt, wire) in [
            (TtsAudioFormat::Mp3, "mp3"),
            (TtsAudioFormat::Opus, "opus"),
            (TtsAudioFormat::Aac, "aac"),
            (TtsAudioFormat::Flac, "flac"),
            (TtsAudioFormat::Wav, "wav"),
            (TtsAudioFormat::Pcm, "pcm"),
        ] {
            assert_eq!(
                serde_json::to_value(fmt).expect("ser"),
                serde_json::json!(wire)
            );
            let back: TtsAudioFormat = serde_json::from_value(serde_json::json!(wire)).expect("de");
            assert_eq!(back, fmt);
            assert_eq!(fmt.as_neutral_str(), wire);
        }
    }

    #[test]
    fn audio_format_mime_mapping() {
        assert_eq!(TtsAudioFormat::Mp3.mime(), "audio/mpeg");
        assert_eq!(TtsAudioFormat::Opus.mime(), "audio/ogg");
        assert_eq!(TtsAudioFormat::Aac.mime(), "audio/aac");
        assert_eq!(TtsAudioFormat::Flac.mime(), "audio/flac");
        assert_eq!(TtsAudioFormat::Wav.mime(), "audio/wav");
        assert_eq!(TtsAudioFormat::Pcm.mime(), "audio/L16");
    }

    #[test]
    fn error_display_is_opaque_and_typed() {
        assert_eq!(
            TtsError::Auth("openai tts authentication failed".to_owned()).to_string(),
            "auth error: openai tts authentication failed"
        );
        assert_eq!(TtsError::Network.to_string(), "network error");
        assert_eq!(TtsError::Decode.to_string(), "decode error");
        assert_eq!(
            TtsError::FormatUnsupported.to_string(),
            "audio format not supported by provider"
        );
        assert_eq!(
            TtsError::InputTooLong {
                len: 9000,
                max: 4096
            }
            .to_string(),
            "input too long: 9000 chars exceeds max 4096"
        );
        assert_eq!(
            TtsError::ModelDiscovery {
                reason: "models endpoint unavailable".to_owned()
            }
            .to_string(),
            "model discovery failed: models endpoint unavailable"
        );
        assert_eq!(TtsError::ModelUnknown.to_string(), "unknown TTS model");
    }

    #[test]
    fn response_debug_hides_audio_bytes() {
        let resp = TtsResponse {
            audio: Arc::from([1_u8, 2, 3, 4].as_slice()),
            mime: "audio/mpeg".to_owned(),
            model: TtsModelId::new("tts-1"),
        };
        let rendered = format!("{resp:?}");
        assert!(rendered.contains("audio_len: 4"), "got: {rendered}");
        assert!(
            !rendered.contains("[1, 2, 3, 4]"),
            "leaked bytes: {rendered}"
        );
        assert_eq!(resp.clone(), resp);
    }

    #[test]
    fn request_carries_neutral_fields() {
        let req = TtsRequest {
            text: "hallo welt".to_owned(),
            model: TtsModelId::new("eleven_multilingual_v2"),
            voice: VoiceId::new("rachel"),
            format: TtsAudioFormat::Mp3,
        };
        assert_eq!(req.clone(), req);
        assert_eq!(req.voice.as_str(), "rachel");
        assert_eq!(req.format, TtsAudioFormat::Mp3);
    }

    struct StubTtsProvider;

    #[async_trait]
    impl TtsProvider for StubTtsProvider {
        async fn synthesize(&self, _req: TtsRequest) -> Result<TtsResponse, TtsError> {
            Err(TtsError::ModelUnknown)
        }

        fn capabilities(&self) -> TtsProviderCapabilities {
            TtsProviderCapabilities::default()
        }

        fn models(&self) -> Vec<TtsModelInfo> {
            vec![TtsModelInfo {
                id: TtsModelId::new("tts-1"),
            }]
        }
    }

    #[tokio::test]
    async fn fetch_models_default_delegates_to_models() {
        let provider = StubTtsProvider;
        let models = provider.fetch_models().await.expect("models");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id.as_str(), "tts-1");
        assert!(!provider.capabilities().streaming);
        provider
            .synthesize(req())
            .await
            .expect_err("stub provider always errors");
    }

    fn req() -> TtsRequest {
        TtsRequest {
            text: "x".to_owned(),
            model: TtsModelId::new("tts-1"),
            voice: VoiceId::new("alloy"),
            format: TtsAudioFormat::Mp3,
        }
    }
}
