//! # crabgent-tool-cache
//!
//! Compaction layer for oversized tool outputs.
//!
//! ## Pattern
//!
//! Large tool outputs (long shell command stdout, multi-megabyte file
//! reads, etc.) burn the LLM context budget without adding signal. This
//! crate compacts those outputs in two steps:
//!
//! 1. [`ToolCacheHook`] watches `after_tool` events. When a tool
//!    returns more than `min_tokens` tokens of textual output and the call
//!    is not itself a `cache_read`, the hook persists the full content
//!    to a [`crabgent_store::ToolCacheStore`] and replaces the inline
//!    `ToolResult` with a compact [`TruncatedOutput`] object plus
//!    retrieval instructions for `cache_read` or background task processing.
//! 2. [`CacheReadTool`] is a regular [`tool::Tool`]
//!    implementation the LLM invokes by name. It looks up the cached
//!    entry by id, supports byte-range slicing, and returns the
//!    requested portion.
//!
//! Prefer [`ToolCacheBuilder`] when installing both pieces together: it wires
//! the hook and tool with one shared session resolver.
//!
//! Both are typed over `C: ToolCacheStore`, so any backend that
//! implements the trait (in-memory, `SQLite`, future Postgres, custom
//! Redis, ...) can plug in without code changes here.

#![forbid(unsafe_code)]

pub mod builder;
pub mod config;
pub mod hook;
pub mod kernel_ext;
pub(crate) mod preview;
pub mod resolver;
pub mod tool;
pub mod truncated;

pub use builder::ToolCacheBuilder;
pub use config::{
    DEFAULT_MIN_TOKENS, DEFAULT_PREVIEW_BYTES, DEFAULT_TTL_HOURS, ToolCacheConfig,
    ToolCacheConfigError,
};
pub use hook::ToolCacheHook;
pub use kernel_ext::KernelBuilderExt;
pub use resolver::{SessionResolver, default_session_id};
pub use tool::{CacheReadTool, DEFAULT_CACHE_READ_LIMIT, MAX_CACHE_READ_LIMIT};
pub use truncated::TruncatedOutput;
