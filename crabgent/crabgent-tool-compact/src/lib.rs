//! # crabgent-tool-compact
//!
//! Recoverable tool-output compaction (token-killer).
//!
//! Fat tool results (long shell stdout, multi-megabyte file reads, verbose
//! MCP payloads) burn the LLM context budget without adding signal. This
//! crate cuts the tokens the model ingests while keeping the full artifact
//! recoverable by construction.
//!
//! ## Pieces
//!
//! 1. [`ToolCompactHook`] watches `after_tool`. For an oversized output it
//!    runs a deterministic safety-floor (tripwire damage-line retention,
//!    dual-signal success gate, secret-leak gate, compute budget) and a
//!    name-keyed semantic filter, stashes the full original output in a
//!    [`crabgent_store::ToolCacheStore`] keyed by a [`RecallHandle`], and
//!    replaces the inline result with `compacted + coverage-footer`.
//! 2. [`RecallTool`] (tool name `recall`, ops `recall_raw` + `expand`) reads
//!    the stash back by handle, with per-call byte caps and pagination.
//!
//! Install both with [`ToolCompactBuilder`], which shares one store,
//! resolver, and auto-disable tracker between the hook and the tool.
//!
//! ## Invariants
//!
//! - Fail-closed: when not registered, or on any internal failure, the raw
//!   output passes through unchanged. The existing per-tool byte caps (bash
//!   200 KB, `read_file` 30 MB, MCP 5 MB) remain the floor.
//! - Deterministic: no LLM call anywhere. Bounded input implies bounded
//!   time; pathological input degrades to raw passthrough.
//! - Secret-safe: a suspected secret-leak short-circuits to raw passthrough
//!   so the compactor never surfaces a secret the raw output would not have
//!   shown. See [`compactor`].
//! - Default OFF: registered explicitly by a deployment via
//!   [`ToolCompactBuilder`].
//!
//! ## Coexistence with `crabgent-tool-cache`
//!
//! [`ToolCompactHook`] and `crabgent-tool-cache`'s `ToolCacheHook` both
//! rewrite output on `after_tool`. Register one OR the other, not both: two
//! `Decision::Replace` hooks on the same result fight. If both are present,
//! `ToolCacheHook` must be configured to skip the `recall` tool name.
//!
//! ## Limitation
//!
//! The auto-disable tracker is in-memory and per-process: a restart resets
//! the per-(run, tool) recall counters.
//!
//! See the `NOTICE` file for the Apache-2.0 attribution of the filter design.

#![forbid(unsafe_code)]

pub mod autodisable;
pub mod budget;
pub mod builder;
pub mod compactor;
pub mod config;
pub mod filters;
pub mod footer;
pub mod handle;
pub mod hook;
pub mod recall;
pub mod session;
pub mod stats;
pub mod success_gate;
pub mod tripwire;

pub use autodisable::AutoDisableTracker;
pub use builder::{KernelBuilderExt, ToolCompactBuilder};
pub use compactor::{Compactor, CompactorVerdict};
pub use config::ToolCompactConfig;
pub use filters::{CompactInput, FilterPlan, ToolOutputCompactor};
pub use footer::render_footer;
pub use handle::{ParseHandleError, RecallHandle};
pub use hook::ToolCompactHook;
pub use recall::{RECALL_TOOL_NAME, RecallTool};
pub use session::{SessionResolver, default_session_id};
pub use stats::CompactionStats;
pub use success_gate::{StructuredSignal, Verdict};
pub use tripwire::TripwireHits;
