//! Provider-neutral forced-alignment surface.
//!
//! Forced alignment is separate from STT and TTS: it takes already known text
//! plus audio and returns timing information for that text in the audio.

use async_trait::async_trait;
use thiserror::Error;

use crate::message::AudioPayload;

/// Abstraction over backends that align known text to audio.
#[async_trait]
pub trait ForcedAlignmentProvider: Send + Sync {
    /// Align the provided text against one validated audio payload.
    async fn align(
        &self,
        req: ForcedAlignmentRequest,
    ) -> Result<ForcedAlignmentResponse, ForcedAlignmentError>;

    /// Provider-wide capability advertisement.
    fn forced_alignment_capabilities(&self) -> ForcedAlignmentProviderCapabilities;
}

/// Request passed to a forced-alignment provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedAlignmentRequest {
    pub payload: AudioPayload,
    pub text: String,
}

/// Complete forced-alignment response.
#[derive(Debug, Clone, PartialEq)]
pub struct ForcedAlignmentResponse {
    pub characters: Vec<ForcedAlignedCharacter>,
    pub words: Vec<ForcedAlignedWord>,
    /// Provider-specific average alignment loss. Lower is better when present.
    pub loss: Option<f32>,
}

/// One aligned character with timing in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct ForcedAlignedCharacter {
    pub text: String,
    pub start: f32,
    pub end: f32,
}

/// One aligned word with timing in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct ForcedAlignedWord {
    pub text: String,
    pub start: f32,
    pub end: f32,
    /// Provider-specific per-word alignment loss. Lower is better when present.
    pub loss: Option<f32>,
}

/// Provider-wide forced-alignment capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ForcedAlignmentProviderCapabilities {
    pub character_timing: bool,
    pub word_timing: bool,
}

/// Errors returned by forced-alignment providers.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ForcedAlignmentError {
    #[error("auth error: {0}")]
    Auth(String),
    #[error("network error")]
    Network,
    #[error("backend error: {0}")]
    Backend(String),
    #[error("decode error")]
    Decode,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    struct StubForcedAlignmentProvider;

    #[async_trait]
    impl ForcedAlignmentProvider for StubForcedAlignmentProvider {
        async fn align(
            &self,
            req: ForcedAlignmentRequest,
        ) -> Result<ForcedAlignmentResponse, ForcedAlignmentError> {
            Ok(ForcedAlignmentResponse {
                characters: vec![ForcedAlignedCharacter {
                    text: req.text,
                    start: 0.0,
                    end: 0.1,
                }],
                words: Vec::new(),
                loss: Some(0.05),
            })
        }

        fn forced_alignment_capabilities(&self) -> ForcedAlignmentProviderCapabilities {
            ForcedAlignmentProviderCapabilities {
                character_timing: true,
                word_timing: true,
            }
        }
    }

    fn assert_object_safe(_provider: Arc<dyn ForcedAlignmentProvider>) {}

    #[tokio::test]
    async fn forced_alignment_provider_is_object_safe() {
        assert_object_safe(Arc::new(StubForcedAlignmentProvider));
        let provider: Arc<dyn ForcedAlignmentProvider> = Arc::new(StubForcedAlignmentProvider);
        let payload = AudioPayload::new(
            b"RIFFfake".to_vec(),
            "audio/wav",
            Some("clip.wav".to_owned()),
        )
        .expect("valid audio payload");

        let response = provider
            .align(ForcedAlignmentRequest {
                payload,
                text: "H".to_owned(),
            })
            .await
            .expect("align");

        assert_eq!(response.characters.len(), 1);
        assert_eq!(response.characters[0].text, "H");
        assert_eq!(response.loss, Some(0.05));
    }

    #[test]
    fn forced_alignment_capabilities_default_closed() {
        let capabilities = ForcedAlignmentProviderCapabilities::default();
        assert!(!capabilities.character_timing);
        assert!(!capabilities.word_timing);
    }
}
