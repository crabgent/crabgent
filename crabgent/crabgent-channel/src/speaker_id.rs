//! Optional speaker identity enrichment for retained voice transcripts.

use async_trait::async_trait;
use crabgent_core::{AudioPayload, SpeakerIdentity, SttResponse, Subject};
use thiserror::Error;

/// Input for one speaker-identification attempt.
#[derive(Debug, Clone)]
pub struct SpeakerIdentificationRequest {
    /// Validated audio payload for the current voice message.
    pub payload: AudioPayload,
    /// The STT response already computed for this payload.
    pub transcription: SttResponse,
    /// Channel speaker subject. Implementations may use it for policy or
    /// per-speaker profile routing.
    pub subject: Subject,
}

/// Deployment-provided speaker recognizer.
///
/// Upstream owns only the generic carrier. Implementations decide whether they
/// use local voiceprints, channel claims, provider metadata, or another
/// site-specific signal.
#[async_trait]
pub trait SpeakerIdentifier: Send + Sync {
    /// Return zero or more identity guesses for this audio clip.
    async fn identify(
        &self,
        req: SpeakerIdentificationRequest,
    ) -> Result<Vec<SpeakerIdentity>, SpeakerIdentificationError>;
}

/// Speaker identification failed.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpeakerIdentificationError {
    /// Backend or local recognizer failure. Details must not contain secrets.
    #[error("speaker identification failed: {0}")]
    Backend(String),
}
