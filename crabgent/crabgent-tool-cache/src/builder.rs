//! Builder that installs the cache hook and reader tool together.

use std::sync::Arc;

use chrono::Duration;
use crabgent_core::{BuilderState, KernelBuilder, Set, Subject};
use crabgent_store::{SessionId, ToolCacheStore};

use crate::config::{
    CACHE_READ_TOOL_NAME, DEFAULT_PREVIEW_BYTES, ToolCacheConfig, ToolCacheConfigError, default_ttl,
};
use crate::hook::ToolCacheHook;
use crate::resolver::{SessionResolver, default_session_resolver};
use crate::tool::CacheReadTool;

/// Installs [`ToolCacheHook`] and [`CacheReadTool`] with one shared resolver.
pub struct ToolCacheBuilder<C: ToolCacheStore> {
    store: Arc<C>,
    resolver: SessionResolver,
    ttl: Duration,
    config: ToolCacheConfig,
    preview_bytes: usize,
}

impl<C: ToolCacheStore> ToolCacheBuilder<C> {
    pub fn new(store: Arc<C>) -> Self {
        // Materialize the BPE table outside the async after_tool hot path.
        crabgent_core::tokens::warmup();

        Self {
            store,
            resolver: default_session_resolver(),
            ttl: default_ttl(),
            config: ToolCacheConfig::default(),
            preview_bytes: DEFAULT_PREVIEW_BYTES,
        }
    }

    #[must_use]
    pub const fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    #[must_use]
    pub const fn with_min_tokens(mut self, tokens: usize) -> Self {
        self.config.min_tokens = tokens;
        self
    }

    pub fn with_tool_override(
        mut self,
        name: &str,
        tokens: usize,
    ) -> Result<Self, ToolCacheConfigError> {
        if name == CACHE_READ_TOOL_NAME {
            return Err(ToolCacheConfigError::CacheReadOverrideForbidden);
        }
        self.config.tool_overrides.insert(name.to_owned(), tokens);
        Ok(self)
    }

    #[must_use]
    pub const fn with_preview_bytes(mut self, bytes: usize) -> Self {
        self.preview_bytes = bytes;
        self
    }

    #[must_use]
    pub fn with_session_resolver<F>(mut self, f: F) -> Self
    where
        F: Fn(&Subject) -> SessionId + Send + Sync + 'static,
    {
        self.resolver = Arc::new(f);
        self
    }
}

impl<C> ToolCacheBuilder<C>
where
    C: ToolCacheStore + 'static,
{
    #[must_use]
    pub fn install<P>(self, builder: KernelBuilder<P, Set>) -> KernelBuilder<P, Set>
    where
        P: BuilderState,
    {
        let policy = Arc::clone(builder.policy_hook());
        let hook = ToolCacheHook::new(Arc::clone(&self.store))
            .with_ttl(self.ttl)
            .with_config(self.config)
            .with_preview_bytes(self.preview_bytes)
            .with_shared_session_resolver(Arc::clone(&self.resolver));
        let tool = CacheReadTool::new(self.store, policy)
            .with_shared_session_resolver(Arc::clone(&self.resolver));
        builder.add_hook(hook).add_tool(tool)
    }
}
