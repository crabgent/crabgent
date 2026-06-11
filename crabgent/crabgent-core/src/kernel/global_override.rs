//! Global model override accessors for [`Kernel`] and [`KernelBuilder`].

use std::sync::Arc;

use crate::kernel::{BuilderState, Kernel, KernelBuilder};
use crate::model::{GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore};

impl Kernel {
    /// Borrow the global model override store.
    #[must_use]
    pub fn global_model_override_store(&self) -> &Arc<dyn GlobalModelOverrideStore> {
        &self.global_override_store
    }

    /// Borrow the global reasoning-effort override store.
    #[must_use]
    pub fn global_reasoning_effort_override_store(
        &self,
    ) -> &Arc<dyn GlobalReasoningEffortOverrideStore> {
        &self.global_reasoning_effort_override_store
    }
}

impl<P: BuilderState, Pol: BuilderState> KernelBuilder<P, Pol> {
    /// Configure the store used to read a global model override before
    /// provider calls.
    #[must_use]
    pub fn with_global_override_store<S>(mut self, store: Arc<S>) -> Self
    where
        S: GlobalModelOverrideStore,
    {
        self.global_override_store = store;
        self
    }

    /// Configure the store used to read a global model override from an
    /// already-erased trait object.
    #[must_use]
    pub fn with_dyn_global_override_store(
        mut self,
        store: Arc<dyn GlobalModelOverrideStore>,
    ) -> Self {
        self.global_override_store = store;
        self
    }

    /// Configure the store used to read a global reasoning-effort override
    /// before provider calls.
    #[must_use]
    pub fn with_global_reasoning_effort_override_store<S>(mut self, store: Arc<S>) -> Self
    where
        S: GlobalReasoningEffortOverrideStore,
    {
        self.global_reasoning_effort_override_store = store;
        self
    }

    /// Configure the store used to read a global reasoning-effort override
    /// from an already-erased trait object.
    #[must_use]
    pub fn with_dyn_global_reasoning_effort_override_store(
        mut self,
        store: Arc<dyn GlobalReasoningEffortOverrideStore>,
    ) -> Self {
        self.global_reasoning_effort_override_store = store;
        self
    }
}
