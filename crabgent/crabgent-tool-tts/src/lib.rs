//! # crabgent-tool-tts
//!
//! Provider-neutral text-to-speech. [`TtsTool`] is an LLM-facing tool that
//! synthesizes speech from text via an injected [`TtsProvider`] and stores
//! the resulting audio in an [`AudioStore`], returning an opaque `audio_ref`
//! handle the chat run can hand to a channel sink.
//!
//! Any [`TtsProvider`] works here: a built-in adapter (`OpenAI`,
//! `ElevenLabs`) or an external one. The tool owns no HTTP timeout, no
//! retries, no circuit breaker; the provider owns its own transport policy.
//! Every recoverable failure degrades to a soft tool result so the run
//! continues, and provider/store error detail is logged, never returned.
//!
//! [`TtsProvider`]: crabgent_core::TtsProvider
//! [`AudioStore`]: crabgent_channel::AudioStore

#![forbid(unsafe_code)]

pub mod error;
pub mod tool;

pub use error::TtsToolError;
pub use tool::{TOOL_NAME, TtsTool};
