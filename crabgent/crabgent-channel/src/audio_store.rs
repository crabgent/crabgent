//! Audio store trait and supporting types for voice-perception retention.
//!
//! Provides `AudioStore` (put/get) plus `AudioStoreError`; the concrete
//! file-system implementation lives in the `file_system` sub-module. The
//! opaque handle type `AudioRef` lives in `crabgent-core` because
//! `ContentBlock::Transcript` embeds it and core must not depend on
//! channel.

pub mod file_system;
pub mod sweeper;

use std::time::Duration;

use crabgent_core::AudioRef;
use thiserror::Error;

/// Errors that can occur when interacting with an audio store.
#[derive(Debug, Error)]
pub enum AudioStoreError {
    /// An I/O error occurred during a store operation.
    #[error("I/O error: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    /// The requested audio was not found in the store.
    #[error("audio not found")]
    NotFound,
    /// The MIME type is not a supported audio type.
    #[error("unsupported audio MIME type")]
    MimeUnsupported,
    /// The payload exceeds the store's configured maximum size.
    #[error("audio payload too large: {size} bytes exceeds {max}")]
    TooLarge { size: usize, max: usize },
}

/// Trait for storing and retrieving retained audio bytes.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// async tasks. All I/O is async (`tokio::fs`); synchronous file
/// operations are forbidden outside `#[cfg(test)]`. Retention is opt-in:
/// a kernel without a wired `AudioStore` keeps no audio (fail-closed).
#[async_trait::async_trait]
pub trait AudioStore: Send + Sync {
    /// Store the given audio bytes and return an opaque reference.
    ///
    /// The `mime` parameter carries the validated MIME type (e.g.
    /// `"audio/ogg"`); implementations derive the file extension from it
    /// but must not trust it for magic-byte checks, which happen upstream
    /// in `AudioValidator`.
    async fn put(&self, bytes: bytes::Bytes, mime: &str) -> Result<AudioRef, AudioStoreError>;

    /// Retrieve previously stored audio bytes and their MIME type.
    async fn get(&self, audio_ref: &AudioRef) -> Result<(bytes::Bytes, String), AudioStoreError>;

    /// Delete retained audio older than `ttl`, returning the count removed.
    ///
    /// Best-effort: implementations skip entries they cannot stat or remove.
    /// The default is a no-op (`Ok(0)`) for stores without expiry (e.g.
    /// in-memory test doubles). `FileSystemAudioStore` overrides it and
    /// [`sweeper::AudioStoreSweeper`] schedules it. No `AudioStore` decorator
    /// exists in the workspace, so this default is never silently swallowed.
    async fn sweep_expired(&self, _ttl: Duration) -> Result<usize, AudioStoreError> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_messages() {
        assert_eq!(AudioStoreError::NotFound.to_string(), "audio not found");
        assert_eq!(
            AudioStoreError::MimeUnsupported.to_string(),
            "unsupported audio MIME type"
        );
        assert_eq!(
            AudioStoreError::TooLarge { size: 30, max: 25 }.to_string(),
            "audio payload too large: 30 bytes exceeds 25"
        );
        let io = AudioStoreError::Io {
            source: std::io::Error::other("disk full"),
        };
        assert_eq!(io.to_string(), "I/O error: disk full");
    }
}
