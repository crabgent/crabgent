//! # crabgent-session
//!
//! Conversation-session persistence on top of [`crabgent_store::SessionStore`].
//!
//! Two surfaces:
//!
//! - [`SessionManager`]: imperative API for callers that drive sessions
//!   themselves (`open` / `append` / `close`). Useful when the kernel runs
//!   embedded in a larger app that owns the session lifecycle.
//! - [`SessionPersistHook`]: a [`crabgent_core::hook::Hook`] that auto-persists
//!   the conversation log on every kernel run. Plug it into the kernel via
//!   `KernelBuilder::hook(...)` and the session stays in sync without
//!   imperative bookkeeping.
//!
//! Both layers are generic over `S: SessionStore`, so any backend
//! (`InMemoryStore`, `SqliteStore`, custom) works without code changes.

#![forbid(unsafe_code)]

pub mod hook;
pub mod manager;

pub use hook::SessionPersistHook;
pub use manager::SessionManager;
