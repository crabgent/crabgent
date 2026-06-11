//! In-memory global override stores.

use std::sync::Mutex;

use async_trait::async_trait;
use crabgent_core::{
    GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, ModelId, ModelOverrideStoreError,
    ReasoningEffort, ReasoningEffortOverrideStoreError,
};

use crate::StoreError;

/// In-memory implementation of [`GlobalModelOverrideStore`].
#[derive(Default)]
pub struct MemoryGlobalOverrideStore {
    model: Mutex<Option<ModelId>>,
    reasoning_effort: Mutex<Option<ReasoningEffort>>,
}

impl MemoryGlobalOverrideStore {
    fn lock_model(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<ModelId>>, ModelOverrideStoreError> {
        self.model
            .lock()
            .map_err(|e| StoreError::backend(format!("global model override mutex poisoned: {e}")))
            .map_err(|e| ModelOverrideStoreError::backend(e.to_string()))
    }

    fn lock_effort(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<ReasoningEffort>>, ReasoningEffortOverrideStoreError>
    {
        self.reasoning_effort
            .lock()
            .map_err(|e| {
                StoreError::backend(format!(
                    "global reasoning effort override mutex poisoned: {e}"
                ))
            })
            .map_err(|e| ReasoningEffortOverrideStoreError::backend(e.to_string()))
    }
}

#[async_trait]
impl GlobalModelOverrideStore for MemoryGlobalOverrideStore {
    async fn get_global_model_override(&self) -> Result<Option<ModelId>, ModelOverrideStoreError> {
        Ok(self.lock_model()?.clone())
    }

    async fn set_global_model_override(
        &self,
        model: &ModelId,
    ) -> Result<(), ModelOverrideStoreError> {
        *self.lock_model()? = Some(model.clone());
        Ok(())
    }

    async fn clear_global_model_override(&self) -> Result<(), ModelOverrideStoreError> {
        *self.lock_model()? = None;
        Ok(())
    }
}

#[async_trait]
impl GlobalReasoningEffortOverrideStore for MemoryGlobalOverrideStore {
    async fn get_global_reasoning_effort_override(
        &self,
    ) -> Result<Option<ReasoningEffort>, ReasoningEffortOverrideStoreError> {
        Ok(*self.lock_effort()?)
    }

    async fn set_global_reasoning_effort_override(
        &self,
        effort: ReasoningEffort,
    ) -> Result<(), ReasoningEffortOverrideStoreError> {
        *self.lock_effort()? = Some(effort);
        Ok(())
    }

    async fn clear_global_reasoning_effort_override(
        &self,
    ) -> Result<(), ReasoningEffortOverrideStoreError> {
        *self.lock_effort()? = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_get_round_trip() {
        let store = MemoryGlobalOverrideStore::default();

        assert!(
            store
                .get_global_model_override()
                .await
                .expect("test result")
                .is_none()
        );
        store
            .set_global_model_override(&ModelId::new("claude"))
            .await
            .expect("test result");
        assert_eq!(
            store
                .get_global_model_override()
                .await
                .expect("test result"),
            Some(ModelId::new("claude"))
        );
    }

    #[tokio::test]
    async fn set_replaces_existing() {
        let store = MemoryGlobalOverrideStore::default();

        store
            .set_global_model_override(&ModelId::new("claude"))
            .await
            .expect("test result");
        store
            .set_global_model_override(&ModelId::new("gpt"))
            .await
            .expect("test result");

        assert_eq!(
            store
                .get_global_model_override()
                .await
                .expect("test result"),
            Some(ModelId::new("gpt"))
        );
    }

    #[tokio::test]
    async fn clear_removes_override() {
        let store = MemoryGlobalOverrideStore::default();

        store
            .set_global_model_override(&ModelId::new("claude"))
            .await
            .expect("test result");
        store
            .clear_global_model_override()
            .await
            .expect("test result");

        assert!(
            store
                .get_global_model_override()
                .await
                .expect("test result")
                .is_none()
        );
    }

    #[tokio::test]
    async fn effort_set_get_clear_round_trip() {
        let store = MemoryGlobalOverrideStore::default();

        assert!(
            store
                .get_global_reasoning_effort_override()
                .await
                .expect("test result")
                .is_none()
        );
        store
            .set_global_reasoning_effort_override(ReasoningEffort::High)
            .await
            .expect("test result");
        assert_eq!(
            store
                .get_global_reasoning_effort_override()
                .await
                .expect("test result"),
            Some(ReasoningEffort::High)
        );
        store
            .clear_global_reasoning_effort_override()
            .await
            .expect("test result");
        assert!(
            store
                .get_global_reasoning_effort_override()
                .await
                .expect("test result")
                .is_none()
        );
    }
}
