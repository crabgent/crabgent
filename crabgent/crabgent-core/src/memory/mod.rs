//! Memory + session-search primitives shared by the kernel, store
//! traits, and tool implementations.
//!
//! - [`MemoryScope`] is the policy-visible scoping vector. It mirrors
//!   `Subject` attributes (`channel`, `conv`, `agent`, `channel_kind`)
//!   so that policy decisions can use the same dimensions the runtime
//!   carries.
//! - [`MemoryId`] is the time-ordered identifier for a memory document.
//! - [`SearchQuery`] is the input bundle for both memory- and
//!   session-search.
//!
//! Storage traits ([`MemoryStore`], `SessionStore::search`) and record
//! types (`MemoryDoc`, `MemoryHit`, `SessionSearchHit`) live in
//! `crabgent-store`, where the persistence error type already lives.
//!
//! [`MemoryStore`]: ../../crabgent_store/memory/trait.MemoryStore.html

pub mod id;
pub mod query;
pub mod scope;

pub use id::{MemoryId, ParseMemoryIdError};
pub use query::{DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT, OwnerMatch, SearchQuery};
pub use scope::MemoryScope;
