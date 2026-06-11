//! Failure modes of the `speak` tool.
//!
//! These carry the source error for the operator log. No variant's message is
//! forwarded to the LLM verbatim: the tool maps each onto an opaque
//! `&'static str` reason (see [`crate::tool`]). Messages stay short and free
//! of credentials or provider response bodies.

use crabgent_channel::AudioStoreError;
use crabgent_core::TtsError;

/// Errors raised while synthesizing and storing speech.
#[derive(Debug, thiserror::Error)]
pub enum TtsToolError {
    /// The TTS provider failed to synthesize the audio.
    #[error("speech synthesis failed")]
    Provider(#[source] TtsError),
    /// The synthesized audio could not be stored.
    #[error("audio store unavailable")]
    Store(#[source] AudioStoreError),
    /// The input text was empty after trimming.
    #[error("input text was empty")]
    InputEmpty,
    /// The input text exceeded the configured character cap.
    #[error("input text too long")]
    InputTooLong {
        /// Length of the rejected input in characters.
        len: usize,
        /// Configured maximum in characters.
        max: usize,
    },
}
