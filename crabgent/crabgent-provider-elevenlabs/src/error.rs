//! `ElevenLabs` provider-local errors.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ElevenLabsError {
    #[error("elevenlabs authentication failed: {0}")]
    Auth(String),
    #[error("elevenlabs network error")]
    Network,
    #[error("elevenlabs backend error: {0}")]
    Backend(String),
    #[error("elevenlabs decode error")]
    Decode,
    #[error("elevenlabs config error: {0}")]
    Config(String),
}
