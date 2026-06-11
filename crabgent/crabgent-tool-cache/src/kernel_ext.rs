use std::sync::Arc;

use crabgent_core::{BuilderState, KernelBuilder, Set};
use crabgent_store::ToolCacheStore;

use crate::ToolCacheBuilder;

pub trait KernelBuilderExt: Sized {
    fn with_tool_cache<C: ToolCacheStore + 'static>(self, store: Arc<C>) -> Self;
}

impl<P> KernelBuilderExt for KernelBuilder<P, Set>
where
    P: BuilderState,
{
    fn with_tool_cache<C: ToolCacheStore + 'static>(self, store: Arc<C>) -> Self {
        ToolCacheBuilder::new(store).install(self)
    }
}
