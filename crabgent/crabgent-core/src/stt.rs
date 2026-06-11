//! Speech-to-text provider surface and model registry.

use std::collections::HashMap;
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::message::AudioPayload;
use crate::newtype::string_newtype;
use crate::voice::AudioEvent;

/// Stream of speech-to-text events returned by [`SttProvider::stream`].
pub type SttEventStream = Pin<Box<dyn Stream<Item = Result<SttEvent, SttError>> + Send>>;

/// Abstraction over speech-to-text backends.
#[async_trait]
pub trait SttProvider: Send + Sync {
    /// Transcribe one validated audio payload.
    async fn transcribe(&self, req: SttRequest) -> Result<SttResponse, SttError>;

    /// Stream transcription events for one validated audio payload.
    async fn stream(&self, req: SttRequest) -> Result<SttEventStream, SttError>;

    /// Provider-wide capability advertisement.
    fn capabilities(&self) -> SttProviderCapabilities;

    /// Models this STT provider can serve.
    fn models(&self) -> Vec<SttModelInfo> {
        Vec::new()
    }

    /// Fetch the provider's current STT model list.
    async fn fetch_models(&self) -> Result<Vec<SttModelInfo>, SttError> {
        Ok(self.models())
    }
}

/// Request passed to an STT provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttRequest {
    pub payload: AudioPayload,
    pub model: SttModelId,
    pub language: Option<String>,
}

/// Complete transcription response.
#[derive(Debug, Clone, PartialEq)]
pub struct SttResponse {
    pub text: String,
    pub model: SttModelId,
    pub segments: Vec<SttSegment>,
    /// Non-lexical audio events (laughter, applause, ...) when the
    /// provider tags them. Empty when unsupported or not requested.
    pub audio_events: Vec<AudioEvent>,
    /// Detected language code when the provider reports one.
    pub language: Option<String>,
}

/// Timestamped transcript segment.
#[derive(Debug, Clone, PartialEq)]
pub struct SttSegment {
    pub start: f32,
    pub end: f32,
    pub text: String,
    /// Provider speaker label for this segment when reported.
    pub speaker_id: Option<String>,
    /// Provider confidence in `[0, 1]` when reported.
    pub confidence: Option<f32>,
    /// Word-level timing when the provider returns it. Empty otherwise.
    pub words: Vec<SttWord>,
}

/// A single transcribed word with timing.
#[derive(Debug, Clone, PartialEq)]
pub struct SttWord {
    pub text: String,
    pub start: f32,
    pub end: f32,
    /// Provider speaker label for this word when diarization is enabled.
    pub speaker_id: Option<String>,
}

/// Streaming STT event.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SttEvent {
    Delta(String),
    Final(SttResponse),
    Error(SttError),
}

/// Errors returned by STT providers.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SttError {
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
    #[error("unknown STT model")]
    ModelUnknown,
}

/// Stable identifier of an STT model.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SttModelId(String);

string_newtype!(trim SttModelId);

/// Metadata for one STT model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttModelInfo {
    pub id: SttModelId,
    pub supports_streaming: bool,
    pub supports_diarization: bool,
}

/// Provider-wide STT capabilities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SttProviderCapabilities {
    pub streaming: bool,
    pub audio: bool,
}

/// Failure when registering a duplicate STT model id.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("duplicate STT model: {0}")]
pub struct DuplicateSttModelError(pub SttModelId);

/// Failure when looking up an unknown STT model id.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("unknown STT model: {0}")]
pub struct UnknownSttModelError(pub SttModelId);

/// In-memory registry of STT model metadata.
#[derive(Debug, Default)]
pub struct SttModelRegistry {
    models: HashMap<SttModelId, SttModelInfo>,
}

impl SttModelRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, info: SttModelInfo) -> Result<(), DuplicateSttModelError> {
        if self.models.contains_key(&info.id) {
            return Err(DuplicateSttModelError(info.id));
        }
        self.models.insert(info.id.clone(), info);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, id: &SttModelId) -> Option<&SttModelInfo> {
        self.models.get(id)
    }

    pub fn require(&self, id: &SttModelId) -> Result<&SttModelInfo, UnknownSttModelError> {
        self.get(id).ok_or_else(|| UnknownSttModelError(id.clone()))
    }

    pub fn list(&self) -> impl Iterator<Item = &SttModelInfo> + '_ {
        self.models.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.models.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> SttModelInfo {
        SttModelInfo {
            id: SttModelId::new(id),
            supports_streaming: true,
            supports_diarization: false,
        }
    }

    #[test]
    fn stt_model_id_trims_whitespace() {
        let id = SttModelId::new(" whisper-1 ");
        assert_eq!(id.as_str(), "whisper-1");
    }

    #[test]
    fn stt_model_id_serde_is_transparent() {
        let id = SttModelId::new("gpt-4o-transcribe");
        let value = serde_json::to_value(&id).expect("ser");
        assert_eq!(value, serde_json::json!("gpt-4o-transcribe"));

        let back: SttModelId = serde_json::from_value(value).expect("de");
        assert_eq!(back, id);
    }

    #[test]
    fn stt_word_constructs_and_compares() {
        let w = SttWord {
            text: "ja".into(),
            start: 0.0,
            end: 0.4,
            speaker_id: Some("speaker_0".into()),
        };
        assert_eq!(w.clone(), w);
        assert_eq!(w.text, "ja");
        assert_eq!(w.speaker_id.as_deref(), Some("speaker_0"));
    }

    #[test]
    fn stt_segment_carries_confidence_and_words() {
        let seg = SttSegment {
            start: 0.0,
            end: 1.0,
            text: "ja super".into(),
            speaker_id: None,
            confidence: Some(0.92),
            words: vec![SttWord {
                text: "ja".into(),
                start: 0.0,
                end: 0.4,
                speaker_id: None,
            }],
        };
        assert_eq!(seg.confidence, Some(0.92));
        assert_eq!(seg.words.len(), 1);
        assert_eq!(seg.clone(), seg);
    }

    #[test]
    fn stt_response_carries_audio_events_and_language() {
        let resp = SttResponse {
            text: "ja super".into(),
            model: SttModelId::new("scribe_v2"),
            segments: Vec::new(),
            audio_events: vec![crate::voice::AudioEvent::new("laughter")],
            language: Some("de".into()),
        };
        assert_eq!(resp.language.as_deref(), Some("de"));
        assert_eq!(resp.audio_events.len(), 1);
        assert_eq!(resp.clone(), resp);
        assert!(format!("{resp:?}").contains("laughter"));
    }

    #[test]
    fn stt_model_registry_inserts_and_requires() {
        let mut registry = SttModelRegistry::new();
        registry.insert(model("scribe_v2")).expect("insert ok");

        let info = registry
            .require(&SttModelId::new("scribe_v2"))
            .expect("model registered");

        assert!(info.supports_streaming);
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());
    }

    #[test]
    fn stt_model_registry_rejects_duplicate() {
        let mut registry = SttModelRegistry::new();
        registry.insert(model("scribe_v2")).expect("insert ok");

        let err = registry
            .insert(model("scribe_v2"))
            .expect_err("duplicate rejected");

        assert_eq!(err.0.as_str(), "scribe_v2");
    }

    #[test]
    fn stt_model_registry_rejects_unknown() {
        let registry = SttModelRegistry::new();

        let err = registry
            .require(&SttModelId::new("missing"))
            .expect_err("unknown rejected");

        assert_eq!(err.0.as_str(), "missing");
    }

    struct StubSttProvider;

    #[async_trait]
    impl SttProvider for StubSttProvider {
        async fn transcribe(&self, _req: SttRequest) -> Result<SttResponse, SttError> {
            Err(SttError::ModelUnknown)
        }

        async fn stream(&self, _req: SttRequest) -> Result<SttEventStream, SttError> {
            Ok(Box::pin(futures::stream::empty()))
        }

        fn capabilities(&self) -> SttProviderCapabilities {
            SttProviderCapabilities::default()
        }

        fn models(&self) -> Vec<SttModelInfo> {
            vec![model("scribe_v2")]
        }
    }

    #[tokio::test]
    async fn stt_provider_fetch_models_default() {
        let provider = StubSttProvider;

        let models = provider.fetch_models().await.expect("models");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id.as_str(), "scribe_v2");
    }

    #[test]
    fn stt_model_discovery_error_display() {
        let err = SttError::ModelDiscovery {
            reason: "models endpoint unavailable".to_owned(),
        };

        assert_eq!(
            err.to_string(),
            "model discovery failed: models endpoint unavailable"
        );
    }
}
