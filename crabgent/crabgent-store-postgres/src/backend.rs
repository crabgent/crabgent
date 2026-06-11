//! Top-level Postgres backend.

use sqlx::PgPool;

use crabgent_core::{
    GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, ModelId, ModelOverrideStoreError,
    ReasoningEffort, ReasoningEffortOverrideStoreError,
};
use crabgent_store::{Store, StoreError};

use crate::config::PostgresStoreConfig;
use crate::cron::PostgresCronStore;
use crate::global_override::PostgresGlobalOverrideStore;
use crate::goal::PostgresGoalStore;
use crate::memory::PostgresMemoryStore;
use crate::pool::build_pool;
use crate::session::PostgresSessionStore;
use crate::task::PostgresTaskStore;
use crate::tool_cache::PostgresToolCacheStore;

/// Postgres-backed store. The sqlx pool is internally shared, so cloning a
/// `PostgresStore` is cheap and safe across threads.
#[derive(Clone)]
pub struct PostgresStore {
    session: PostgresSessionStore,
    memory: PostgresMemoryStore,
    task: PostgresTaskStore,
    cron: PostgresCronStore,
    tool_cache: PostgresToolCacheStore,
    goal: PostgresGoalStore,
    global_override: PostgresGlobalOverrideStore,
}

impl PostgresStore {
    /// Open a Postgres store and apply embedded migrations.
    pub async fn open(config: PostgresStoreConfig) -> Result<Self, StoreError> {
        let pool = build_pool(&config).await?;
        Ok(Self::from_pool(pool))
    }

    /// Wrap an existing migrated pool. Intended for tests and advanced
    /// consumers that manage migrations externally.
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            session: PostgresSessionStore::new(pool.clone()),
            memory: PostgresMemoryStore::new(pool.clone()),
            task: PostgresTaskStore::new(pool.clone()),
            cron: PostgresCronStore::new(pool.clone()),
            tool_cache: PostgresToolCacheStore::new(pool.clone()),
            goal: PostgresGoalStore::new(pool.clone()),
            global_override: PostgresGlobalOverrideStore::new(pool),
        }
    }

    /// Borrow the session sub-store.
    #[must_use]
    pub const fn session_store(&self) -> &PostgresSessionStore {
        &self.session
    }

    /// Borrow the memory sub-store.
    #[must_use]
    pub const fn memory_store(&self) -> &PostgresMemoryStore {
        &self.memory
    }

    /// Borrow the task sub-store.
    #[must_use]
    pub const fn task_store(&self) -> &PostgresTaskStore {
        &self.task
    }

    /// Borrow the cron sub-store.
    #[must_use]
    pub const fn cron_store(&self) -> &PostgresCronStore {
        &self.cron
    }

    /// Borrow the tool-cache sub-store.
    #[must_use]
    pub const fn tool_cache_store(&self) -> &PostgresToolCacheStore {
        &self.tool_cache
    }

    /// Borrow the goal sub-store.
    #[must_use]
    pub const fn goal_store(&self) -> &PostgresGoalStore {
        &self.goal
    }

    /// Borrow the global model override sub-store.
    #[must_use]
    pub const fn global_override_store(&self) -> &PostgresGlobalOverrideStore {
        &self.global_override
    }
}

impl Store for PostgresStore {
    type Session = PostgresSessionStore;
    type Memory = PostgresMemoryStore;
    type Task = PostgresTaskStore;
    type Cron = PostgresCronStore;
    type ToolCache = PostgresToolCacheStore;
    type Goal = PostgresGoalStore;

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
impl GlobalModelOverrideStore for PostgresStore {
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
impl GlobalReasoningEffortOverrideStore for PostgresStore {
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
