//! # crabgent-store-sqlite
//!
//! SQLite-backed implementation of every [`crabgent_store`] trait. The
//! database file is configured via [`SqliteStore::open`]. Migrations are
//! embedded and applied on open.

mod backend;
mod config;
mod cron;
mod fts;
mod global_override;
mod goal;
mod memory;
mod retry;
mod session;
mod sqlite_vec;
mod task;
mod tool_cache;

pub use backend::{SqliteStore, SqliteStoreError};
pub use config::{DEFAULT_EMBEDDING_DIM, SqliteStoreConfig};
pub use cron::SqliteCronStore;
pub use global_override::SqliteGlobalOverrideStore;
pub use goal::SqliteGoalStore;
pub use memory::SqliteMemoryStore;
pub use session::SqliteSessionStore;
#[cfg(any(test, debug_assertions))]
pub use session::pause_after_select_miss::{
    PauseAfterSelectMissGuard, arm_pause_after_select_miss,
};
pub use task::SqliteTaskStore;
pub use tool_cache::SqliteToolCacheStore;
