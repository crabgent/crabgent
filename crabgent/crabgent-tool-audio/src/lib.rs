//! # crabgent-tool-audio
//!
//! The A-path of the voice-perception epic. [`HearAgainTool`] is an
//! on-demand pull tool: it takes the `AudioRef` of a retained user voice
//! message, fetches the raw audio from an [`AudioStore`], and sends a
//! one-shot `[audio + question]` request to an independent audio-native
//! model (e.g. `gpt-4o-audio-preview`). The model issuing the chat turn
//! need not be audio-capable; the audio model answers questions about HOW
//! the message was said (tone, pauses, emphasis, mumbled words) that the
//! text transcript may have lost.
//!
//! The chat model never sees the `AudioRef` directly: provider projection
//! strips `source_audio` from the prompt. [`AudioHintHook`] surfaces the
//! handle in a trust-fenced `before_llm` note so the chat model can pass it
//! back to `hear_again`.
//!
//! [`AudioStore`]: crabgent_channel::AudioStore

#![forbid(unsafe_code)]

pub mod call;
pub mod circuit;
pub mod hook;
pub mod tool;
pub mod transcode;

pub use call::{AskAudioError, AudioAnswer, AudioCall, ask_audio};
pub use circuit::{AudioCircuit, AudioCircuitConfig};
pub use hook::AudioHintHook;
pub use tool::HearAgainTool;
