//! Builder that installs the compaction hook and recall tool together.
//!
//! Both pieces share one store, one session resolver, and one auto-disable
//! tracker, so the hook's stash and the recall tool's lookup agree on keys and
//! the recall counter feeds back into the hook's auto-disable decision.

use std::sync::Arc;

use crabgent_core::{BuilderState, KernelBuilder, Set, Subject};
use crabgent_store::{SessionId, ToolCacheStore};

use crate::autodisable::AutoDisableTracker;
use crate::config::ToolCompactConfig;
use crate::hook::ToolCompactHook;
use crate::recall::RecallTool;
use crate::session::{SessionResolver, default_session_resolver};

/// Installs [`ToolCompactHook`] and [`RecallTool`] with shared state.
///
/// Register this OR `crabgent-tool-cache`'s `ToolCacheBuilder`, not both: both
/// rewrite output on `after_tool` and would fight over the same result.
pub struct ToolCompactBuilder<C: ToolCacheStore> {
    store: Arc<C>,
    config: ToolCompactConfig,
    resolver: SessionResolver,
}

impl<C: ToolCacheStore> ToolCompactBuilder<C> {
    /// Start a builder over a store backend.
    pub fn new(store: Arc<C>) -> Self {
        // Materialize the token estimator outside the async after_tool path.
        crabgent_core::tokens::warmup();
        Self {
            store,
            config: ToolCompactConfig::default(),
            resolver: default_session_resolver(),
        }
    }

    /// Replace the tunables.
    #[must_use]
    pub const fn with_config(mut self, config: ToolCompactConfig) -> Self {
        self.config = config;
        self
    }

    /// Override the compaction token threshold.
    #[must_use]
    pub const fn with_min_tokens(mut self, tokens: usize) -> Self {
        self.config = self.config.with_min_tokens(tokens);
        self
    }

    /// Install a custom subject-to-session resolver, shared by hook and tool.
    #[must_use]
    pub fn with_session_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn(&Subject) -> SessionId + Send + Sync + 'static,
    {
        self.resolver = Arc::new(resolver);
        self
    }
}

impl<C> ToolCompactBuilder<C>
where
    C: ToolCacheStore + 'static,
{
    /// Register the hook and the recall tool on a kernel builder.
    #[must_use]
    pub fn install<P>(self, builder: KernelBuilder<P, Set>) -> KernelBuilder<P, Set>
    where
        P: BuilderState,
    {
        let policy = Arc::clone(builder.policy_hook());
        let tracker = AutoDisableTracker::new();
        let hook = ToolCompactHook::new(Arc::clone(&self.store), self.config.clone())
            .with_tracker(tracker.clone())
            .with_shared_session_resolver(Arc::clone(&self.resolver));
        let tool = RecallTool::new(self.store, policy, tracker)
            .with_shared_session_resolver(Arc::clone(&self.resolver))
            .with_limits(
                self.config.recall_default_limit,
                self.config.recall_max_limit,
            );
        builder.add_hook(hook).add_tool(tool)
    }
}

/// Ergonomic kernel-builder extension: `.with_tool_compact(store)`.
pub trait KernelBuilderExt: Sized {
    /// Install compaction with default tunables.
    fn with_tool_compact<C: ToolCacheStore + 'static>(self, store: Arc<C>) -> Self;
}

impl<P> KernelBuilderExt for KernelBuilder<P, Set>
where
    P: BuilderState,
{
    fn with_tool_compact<C: ToolCacheStore + 'static>(self, store: Arc<C>) -> Self {
        ToolCompactBuilder::new(store).install(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_store::memory::MemoryToolCacheStore;

    #[test]
    fn builder_applies_config_overrides() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let builder = ToolCompactBuilder::new(store).with_min_tokens(7);
        assert_eq!(builder.config.min_tokens, 7);
    }

    #[test]
    fn builder_accepts_full_config_and_custom_resolver() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let config = ToolCompactConfig::default().with_autodisable_n(9);
        let builder = ToolCompactBuilder::new(store)
            .with_config(config)
            .with_session_resolver(|_subject| SessionId::new());
        assert_eq!(builder.config.autodisable_n, 9);
    }
}
