//! # crabgent-tool-session
//!
//! [`SessionSearchTool`] exposes session-history search to the LLM.
//! Backends with FTS support (e.g. `SqliteSessionStore`) return ranked
//! hits with excerpts; backends without indexing return empty results
//! via the trait's default impl.
//!
//! Policy-gated: every call goes through the configured `PolicyHook`
//! with a typed `Action::SessionSearch { query, scope }` so the policy
//! can enforce per-subject and per-scope rules.

#![forbid(unsafe_code)]

pub mod tool;

pub use tool::SessionSearchTool;
