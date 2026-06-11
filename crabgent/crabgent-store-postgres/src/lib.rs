//! # crabgent-store-postgres
//!
//! Postgres-backed implementation scaffold for the `crabgent_store` traits.
//! Migrations are embedded and applied when [`PostgresStore::open`] builds a
//! pool.

extern crate self as crabgent_store_postgres;

mod backend;
mod config;
mod error;
mod fts;
mod global_override;
mod pgvector_migrate;
mod pool;
mod retry;

pub mod cron;
pub mod goal;
pub mod memory;
pub mod session;
pub mod task;
pub mod tool_cache;

#[cfg(any(test, feature = "test-support"))]
pub mod test_helpers;

pub use backend::PostgresStore;
pub use config::{DEFAULT_EMBEDDING_DIM, PostgresStoreConfig, PostgresStoreConfigBuilder};
pub use error::PostgresStoreError;
pub use global_override::PostgresGlobalOverrideStore;
pub use goal::PostgresGoalStore;
pub use memory::PostgresMemoryStore;
pub use session::PostgresSessionStore;
#[cfg(any(test, debug_assertions))]
pub use session::pause_after_select_miss::{
    PauseAfterSelectMissGuard, arm_pause_after_select_miss,
};
pub use task::PostgresTaskStore;
pub use tool_cache::PostgresToolCacheStore;
