//! In-memory implementation of every [`crate::Store`] sub-trait.
//!
//! Intended for tests and solo runs that do not need persistence across
//! restarts. Behaviour matches the SQLite/Postgres backends as closely as
//! possible (insertion order, claim semantics, cleanup cutoffs).

mod cron;
mod global_override;
mod goal;
mod memory_store;
mod session;
mod task;
mod tool_cache;

use async_trait::async_trait;
use crabgent_core::{
    GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, ModelId, ModelOverrideStoreError,
    ReasoningEffort, ReasoningEffortOverrideStoreError,
};

use crate::traits::Store;

pub use cron::MemoryCronStore;
pub use global_override::MemoryGlobalOverrideStore;
pub use goal::MemoryGoalStore;
pub use memory_store::MemoryMemoryStore;
pub use session::MemorySessionStore;
pub use task::MemoryTaskStore;
pub use tool_cache::MemoryToolCacheStore;

/// Default in-memory backend implementing every store sub-trait.
///
/// Renamed from `MemoryStore` in Initial design8 to free that name for the
/// new long-term-fact `MemoryStore` trait. Behaviour unchanged.
#[derive(Default)]
pub struct InMemoryStore {
    session: MemorySessionStore,
    task: MemoryTaskStore,
    cron: MemoryCronStore,
    tool_cache: MemoryToolCacheStore,
    memory: MemoryMemoryStore,
    goal: MemoryGoalStore,
    global_override: MemoryGlobalOverrideStore,
}

impl InMemoryStore {
    /// Construct a fresh empty backend.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow the in-memory [`MemoryMemoryStore`] backend.
    #[must_use]
    pub const fn memory(&self) -> &MemoryMemoryStore {
        &self.memory
    }

    /// Borrow the in-memory global model override store.
    #[must_use]
    pub const fn global_override(&self) -> &MemoryGlobalOverrideStore {
        &self.global_override
    }
}

impl Store for InMemoryStore {
    type Session = MemorySessionStore;
    type Memory = MemoryMemoryStore;
    type Task = MemoryTaskStore;
    type Cron = MemoryCronStore;
    type ToolCache = MemoryToolCacheStore;
    type Goal = MemoryGoalStore;

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

#[async_trait]
impl GlobalModelOverrideStore for InMemoryStore {
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

#[async_trait]
impl GlobalReasoningEffortOverrideStore for InMemoryStore {
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

    #[test]
    fn new_returns_empty_store() {
        let store = InMemoryStore::new();
        let _ = store.session();
        let _ = Store::memory(&store);
        let _ = store.task();
        let _ = store.cron();
        let _ = store.tool_cache();
    }

    #[test]
    fn default_matches_new() {
        let _: InMemoryStore = InMemoryStore::default();
    }

    #[test]
    fn store_provides_memory_store_via_trait() {
        let store = InMemoryStore::new();
        let memory: &MemoryMemoryStore = Store::memory(&store);
        assert!(std::ptr::eq(memory, store.memory()));
    }
}
