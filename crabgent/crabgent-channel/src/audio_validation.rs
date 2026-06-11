//! Audio payload validation for channel adapters.

use thiserror::Error;

/// Maximum decoded audio payload size accepted by channel adapters.
pub const MAX_AUDIO_BYTES: u64 = 25_000_000;
const MAX_AUDIO_LEN_BYTES: usize = 25_000_000;

/// MIME types accepted for inbound speech-to-text audio payloads.
pub const ALLOWED_AUDIO_MIMES: &[&str] = &[
    "audio/ogg",
    "audio/mpeg",
    "audio/mp3",
    "audio/mp4",
    "audio/x-m4a",
    "audio/wav",
    "audio/webm",
    "audio/flac",
    "audio/x-flac",
    "audio/opus",
];

/// Validates audio payload bytes before they enter the kernel surface.
pub struct AudioValidator {
    infer: infer::Infer,
}

impl AudioValidator {
    pub const fn new() -> Self {
        Self {
            infer: infer::Infer::new(),
        }
    }

    /// Validate payload size, claimed MIME allowlist membership and magic bytes.
    pub fn validate(&self, bytes: &[u8], claimed_mime: &str) -> Result<(), AudioRejection> {
        if bytes.len() > MAX_AUDIO_LEN_BYTES {
            return Err(AudioRejection::TooLarge);
        }
        if !is_allowed_mime(claimed_mime) {
            return Err(AudioRejection::UnsupportedMime);
        }

        let detected = self
            .infer
            .get(bytes)
            .ok_or(AudioRejection::InvalidBytes)?
            .mime_type();
        if mime_matches(claimed_mime, detected) {
            Ok(())
        } else {
            Err(AudioRejection::MagicByteMismatch)
        }
    }
}

impl Default for AudioValidator {
    fn default() -> Self {
        Self::new()
    }
}

/// Reason an inbound audio payload was rejected.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudioRejection {
    #[error("audio payload too large")]
    TooLarge,
    #[error("unsupported audio MIME type")]
    UnsupportedMime,
    #[error("invalid audio bytes")]
    InvalidBytes,
    #[error("audio magic bytes do not match claimed MIME type")]
    MagicByteMismatch,
}

fn is_allowed_mime(mime: &str) -> bool {
    ALLOWED_AUDIO_MIMES.contains(&mime)
}

fn mime_matches(claimed: &str, detected: &str) -> bool {
    claimed == detected
        || matches!(
            (claimed, detected),
            ("audio/mp3", "audio/mpeg")
                | ("audio/mpeg", "audio/mp3")
                | ("audio/wav", "audio/x-wav")
                | ("audio/x-wav", "audio/wav")
                | ("audio/flac", "audio/x-flac")
                | ("audio/x-flac", "audio/flac")
                | ("audio/x-m4a" | "audio/mp4", "audio/m4a" | "video/mp4")
                | ("audio/webm", "video/webm")
                | ("audio/opus", "audio/ogg")
                | ("audio/ogg", "audio/opus")
        )
}
