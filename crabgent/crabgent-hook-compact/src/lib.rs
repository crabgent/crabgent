//! # crabgent-hook-compact
//!
//! Semantic context-window compaction as a [`Hook`].
//!
//! [`CompactHook`] runs in the `pre_compact` slot. When the typed
//! conversation exceeds configured message or token thresholds, it asks a
//! caller-supplied summary-provider fallback chain to summarize the older part
//! of the conversation, then returns a provider-facing message list made from:
//!
//! 1. leading system messages, preserved verbatim;
//! 2. one generated compact-summary user-visible context message;
//! 3. the most recent tail messages, preserved verbatim.
//!
//! Current kernels apply this hook's replacement to the canonical message log
//! before the assistant response append, so persistence hooks observe the
//! compacted view. Non-kernel callers can use
//! [`CompactHook::compact_session`] to apply the same compaction to a stored
//! session.
//!
//! [`Hook`]: crabgent_core::Hook
//! [`Provider`]: crabgent_core::Provider

#![forbid(unsafe_code)]

mod compact_plan;
pub mod compact_session;
mod config;
mod hook;
mod render;
mod summary_chain;
pub mod token_count;

pub use config::{CompactConfig, CompactFailureMode};
pub use hook::CompactHook;
pub use summary_chain::CompactError;
