//! Top-level `SQLite` backend. Owns the [`SqlitePool`] and exposes the
//! five sub-stores via the [`Store`] trait.

use std::path::Path;

use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{ConnectOptions, SqlitePool};
use thiserror::Error;

use crabgent_core::{
    GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, ModelId, ModelOverrideStoreError,
    ReasoningEffort, ReasoningEffortOverrideStoreError,
};
use crabgent_log::LogLevelFilter;
use crabgent_store::Store;

use crate::config::SqliteStoreConfig;
use crate::cron::SqliteCronStore;
use crate::global_override::SqliteGlobalOverrideStore;
use crate::goal::SqliteGoalStore;
use crate::memory::SqliteMemoryStore;
use crate::session::SqliteSessionStore;
use crate::sqlite_vec::{register_sqlite_vec_connection, register_sqlite_vec_once};
use crate::task::SqliteTaskStore;
use crate::tool_cache::SqliteToolCacheStore;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Errors emitted while opening or migrating the `SQLite` store.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SqliteStoreError {
    /// Pool failed to initialise (file lock, permissions, malformed URL, ...).
    #[error("connect failed: {0}")]
    Connect(#[from] sqlx::Error),

    /// Schema migration failed mid-run.
    #[error("migration failed: {0}")]
    Migration(String),
}

/// `SQLite`-backed [`Store`]. The pool is internally `Arc`'d, so cloning a
/// `SqliteStore` is cheap and safe across threads.
#[derive(Clone)]
pub struct SqliteStore {
    session: SqliteSessionStore,
    task: SqliteTaskStore,
    cron: SqliteCronStore,
    tool_cache: SqliteToolCacheStore,
    memory: SqliteMemoryStore,
    goal: SqliteGoalStore,
    global_override: SqliteGlobalOverrideStore,
}

impl SqliteStore {
    /// Open a `SQLite` store at `path`, creating the file if needed and
    /// applying embedded migrations.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, SqliteStoreError> {
        Self::open_with_config(path, SqliteStoreConfig::default()).await
    }

    /// Open a `SQLite` store with explicit construction config.
    pub async fn open_with_config(
        path: impl AsRef<Path>,
        config: SqliteStoreConfig,
    ) -> Result<Self, SqliteStoreError> {
        register_sqlite_vec_once().map_err(|e| SqliteStoreError::Migration(e.to_string()))?;
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .log_statements(LogLevelFilter::Debug);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        Self::from_pool_with_config(pool, config).await
    }

    /// Open an in-memory `SQLite` store, primarily for tests.
    pub async fn open_in_memory() -> Result<Self, SqliteStoreError> {
        Self::open_in_memory_with_config(SqliteStoreConfig::default()).await
    }

    /// Open an in-memory `SQLite` store with explicit construction config.
    pub async fn open_in_memory_with_config(
        config: SqliteStoreConfig,
    ) -> Result<Self, SqliteStoreError> {
        register_sqlite_vec_once().map_err(|e| SqliteStoreError::Migration(e.to_string()))?;
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .shared_cache(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        Self::from_pool_with_config(pool, config).await
    }

    /// Wrap an existing pool. Applies migrations.
    ///
    /// `register_sqlite_vec_once` is called here as the canonical safety net
    /// for external callers that pass their own `SqlitePool` without going
    /// through `open` / `open_in_memory`. The two `open*` entry points also
    /// pre-register so a misconfigured environment fails before the pool is
    /// allocated; the inner `OnceLock` makes the second call a constant-time
    /// no-op, so the duplication is intentional rather than an oversight.
    pub async fn from_pool(pool: SqlitePool) -> Result<Self, SqliteStoreError> {
        Self::from_pool_with_config(pool, SqliteStoreConfig::default()).await
    }

    /// Wrap an existing pool with explicit construction config.
    pub async fn from_pool_with_config(
        pool: SqlitePool,
        config: SqliteStoreConfig,
    ) -> Result<Self, SqliteStoreError> {
        register_sqlite_vec_once().map_err(|e| SqliteStoreError::Migration(e.to_string()))?;
        Self::run_migrations(&pool).await?;
        Self::ensure_memory_vec(&pool, config.embedding_dim()).await?;
        Ok(Self {
            session: SqliteSessionStore::new(pool.clone()),
            task: SqliteTaskStore::new(pool.clone()),
            cron: SqliteCronStore::new(pool.clone()),
            tool_cache: SqliteToolCacheStore::new(pool.clone()),
            memory: SqliteMemoryStore::new(pool.clone()),
            goal: SqliteGoalStore::new(pool.clone()),
            global_override: SqliteGlobalOverrideStore::new(pool),
        })
    }

    /// Borrow the [`SqliteMemoryStore`] for long-term-fact persistence.
    #[must_use]
    pub const fn memory(&self) -> &SqliteMemoryStore {
        &self.memory
    }

    /// Borrow the global model override sub-store.
    #[must_use]
    pub const fn global_override(&self) -> &SqliteGlobalOverrideStore {
        &self.global_override
    }

    async fn run_migrations(pool: &SqlitePool) -> Result<(), SqliteStoreError> {
        MIGRATOR
            .run(pool)
            .await
            .map_err(|e| SqliteStoreError::Migration(e.to_string()))
    }

    async fn ensure_memory_vec(pool: &SqlitePool, dim: usize) -> Result<(), SqliteStoreError> {
        if dim == 0 {
            return Err(SqliteStoreError::Migration(
                "embedding dimension must be positive".to_owned(),
            ));
        }
        let sql = format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vec \
             USING vec0(memory_id TEXT PRIMARY KEY, embedding FLOAT[{dim}] distance_metric=cosine)"
        );
        let mut conn = pool
            .acquire()
            .await
            .map_err(|e| SqliteStoreError::Migration(e.to_string()))?;
        register_sqlite_vec_connection(&mut conn)
            .await
            .map_err(|e| SqliteStoreError::Migration(e.to_string()))?;
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&mut *conn)
            .await
            .map_err(|e| SqliteStoreError::Migration(e.to_string()))?;
        Ok(())
    }
}

impl Store for SqliteStore {
    type Session = SqliteSessionStore;
    type Memory = SqliteMemoryStore;
    type Task = SqliteTaskStore;
    type Cron = SqliteCronStore;
    type ToolCache = SqliteToolCacheStore;
    type Goal = SqliteGoalStore;

    fn session(&self) -> &Self::Session {
        &self.session
    }

    fn memory(&self) -> &Self::Memory {
        &self.memory
    }

    fn task(&self) -> &Self::Task {
        &self.task
    }

    fn cron(&self) -> &Self::Cron {
        &self.cron
    }

    fn tool_cache(&self) -> &Self::ToolCache {
        &self.tool_cache
    }

    fn goal(&self) -> &Self::Goal {
        &self.goal
    }
}

#[async_trait::async_trait]
impl GlobalModelOverrideStore for SqliteStore {
    async fn get_global_model_override(&self) -> Result<Option<ModelId>, ModelOverrideStoreError> {
        self.global_override.get_global_model_override().await
    }

    async fn set_global_model_override(
        &self,
        model: &ModelId,
    ) -> Result<(), ModelOverrideStoreError> {
        self.global_override.set_global_model_override(model).await
    }

    async fn clear_global_model_override(&self) -> Result<(), ModelOverrideStoreError> {
        self.global_override.clear_global_model_override().await
    }
}

#[async_trait::async_trait]
impl GlobalReasoningEffortOverrideStore for SqliteStore {
    async fn get_global_reasoning_effort_override(
        &self,
    ) -> Result<Option<ReasoningEffort>, ReasoningEffortOverrideStoreError> {
        self.global_override
            .get_global_reasoning_effort_override()
            .await
    }

    async fn set_global_reasoning_effort_override(
        &self,
        effort: ReasoningEffort,
    ) -> Result<(), ReasoningEffortOverrideStoreError> {
        self.global_override
            .set_global_reasoning_effort_override(effort)
            .await
    }

    async fn clear_global_reasoning_effort_override(
        &self,
    ) -> Result<(), ReasoningEffortOverrideStoreError> {
        self.global_override
            .clear_global_reasoning_effort_override()
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    #[tokio::test]
    async fn open_in_memory_runs_migrations() {
        let store = SqliteStore::open_in_memory().await.expect("open");
        let _ = store.session();
        let _ = Store::memory(&store);
        let _ = store.task();
        let _ = store.cron();
        let _ = store.tool_cache();
    }

    #[tokio::test]
    async fn clone_shares_pool() {
        let store = SqliteStore::open_in_memory().await.expect("open");
        let cloned: SqliteStore = store.clone();
        let _ = cloned.session();
        let _ = store.session();
    }

    #[tokio::test]
    async fn store_provides_memory_store_via_trait() {
        let store = SqliteStore::open_in_memory().await.expect("open");
        let memory: &SqliteMemoryStore = Store::memory(&store);
        assert!(std::ptr::eq(memory, store.memory()));
    }

    async fn memory_pool() -> SqlitePool {
        register_sqlite_vec_once().expect("register sqlite-vec");
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .shared_cache(true);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .expect("open test pool")
    }

    async fn migration_versions(pool: &SqlitePool) -> Vec<(i64, i64)> {
        sqlx::query(
            "SELECT version, COUNT(*) AS count \
             FROM _sqlx_migrations \
             GROUP BY version \
             ORDER BY version",
        )
        .fetch_all(pool)
        .await
        .expect("read migration tracking")
        .into_iter()
        .map(|row| (row.get::<i64, _>("version"), row.get::<i64, _>("count")))
        .collect()
    }

    async fn memory_vec_sql(pool: &SqlitePool) -> String {
        sqlx::query_scalar::<_, String>(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'memory_vec'",
        )
        .fetch_one(pool)
        .await
        .expect("read memory_vec schema")
    }

    #[tokio::test]
    async fn migration_tracking_table_records_each_migration_once() {
        let pool = memory_pool().await;

        SqliteStore::from_pool(pool.clone()).await.expect("migrate");

        assert_eq!(
            migration_versions(&pool).await,
            vec![
                (1, 1),
                (2, 1),
                (3, 1),
                (4, 1),
                (5, 1),
                (6, 1),
                (7, 1),
                (8, 1),
                (9, 1),
                (10, 1),
                (11, 1),
                (12, 1),
                (13, 1),
                (14, 1),
                (15, 1),
                (16, 1),
                (17, 1)
            ]
        );
    }

    #[tokio::test]
    async fn migrations_are_skipped_when_already_applied() {
        let pool = memory_pool().await;

        SqliteStore::from_pool(pool.clone())
            .await
            .expect("first migrate");
        SqliteStore::from_pool(pool.clone())
            .await
            .expect("second migrate");

        assert_eq!(
            migration_versions(&pool).await,
            vec![
                (1, 1),
                (2, 1),
                (3, 1),
                (4, 1),
                (5, 1),
                (6, 1),
                (7, 1),
                (8, 1),
                (9, 1),
                (10, 1),
                (11, 1),
                (12, 1),
                (13, 1),
                (14, 1),
                (15, 1),
                (16, 1),
                (17, 1)
            ]
        );
    }

    #[tokio::test]
    async fn custom_embedding_dim_creates_memory_vec_table() {
        let pool = memory_pool().await;
        let config = SqliteStoreConfig::default().with_embedding_dim(8);

        SqliteStore::from_pool_with_config(pool.clone(), config)
            .await
            .expect("migrate with custom vector dim");

        assert!(memory_vec_sql(&pool).await.contains("FLOAT[8]"));
    }

    #[tokio::test]
    async fn zero_embedding_dim_is_rejected() {
        let pool = memory_pool().await;
        let config = SqliteStoreConfig::default().with_embedding_dim(0);
        let Err(err) = SqliteStore::from_pool_with_config(pool, config).await else {
            panic!("zero vector dim must fail");
        };

        assert!(
            err.to_string()
                .contains("embedding dimension must be positive"),
            "unexpected error: {err}"
        );
    }
}
