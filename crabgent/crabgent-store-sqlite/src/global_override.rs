//! SQLite-backed global model override store.

use async_trait::async_trait;
use std::str::FromStr;

use crabgent_core::{
    GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, ModelId, ModelOverrideStoreError,
    ReasoningEffort, ReasoningEffortOverrideStoreError,
};
use sqlx::Row;
use sqlx::sqlite::SqlitePool;

use crate::retry::retry_transient;

/// `SQLite` implementation of [`GlobalModelOverrideStore`].
#[derive(Clone)]
pub struct SqliteGlobalOverrideStore {
    pool: SqlitePool,
}

impl SqliteGlobalOverrideStore {
    pub(crate) const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn map_err(err: &crabgent_store::StoreError) -> ModelOverrideStoreError {
    ModelOverrideStoreError::backend(err.to_string())
}

fn map_effort_err(err: &crabgent_store::StoreError) -> ReasoningEffortOverrideStoreError {
    ReasoningEffortOverrideStoreError::backend(err.to_string())
}

#[async_trait]
impl GlobalModelOverrideStore for SqliteGlobalOverrideStore {
    async fn get_global_model_override(&self) -> Result<Option<ModelId>, ModelOverrideStoreError> {
        let row = retry_transient("global_model_override.get", || async {
            sqlx::query("SELECT model_id FROM global_model_overrides WHERE singleton = 0")
                .fetch_optional(&self.pool)
                .await
        })
        .await
        .map_err(|err| map_err(&err))?;
        Ok(row.map(|row| {
            let model: String = row.get("model_id");
            ModelId::new(model)
        }))
    }

    async fn set_global_model_override(
        &self,
        model: &ModelId,
    ) -> Result<(), ModelOverrideStoreError> {
        retry_transient("global_model_override.set", || async {
            sqlx::query(
                "INSERT INTO global_model_overrides (singleton, model_id) VALUES (0, ?) \
                 ON CONFLICT(singleton) DO UPDATE SET model_id = excluded.model_id",
            )
            .bind(model.as_str())
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
        .map_err(|err| map_err(&err))
    }

    async fn clear_global_model_override(&self) -> Result<(), ModelOverrideStoreError> {
        retry_transient("global_model_override.clear", || async {
            sqlx::query("DELETE FROM global_model_overrides WHERE singleton = 0")
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
        .map_err(|err| map_err(&err))
    }
}

#[async_trait]
impl GlobalReasoningEffortOverrideStore for SqliteGlobalOverrideStore {
    async fn get_global_reasoning_effort_override(
        &self,
    ) -> Result<Option<ReasoningEffort>, ReasoningEffortOverrideStoreError> {
        let row = retry_transient("global_reasoning_effort_override.get", || async {
            sqlx::query(
                "SELECT reasoning_effort FROM global_reasoning_effort_overrides WHERE singleton = 0",
            )
            .fetch_optional(&self.pool)
            .await
        })
        .await
        .map_err(|err| map_effort_err(&err))?;
        row.map(|row| {
            let effort: String = row.get("reasoning_effort");
            ReasoningEffort::from_str(&effort)
                .map_err(|err| ReasoningEffortOverrideStoreError::backend(err.to_string()))
        })
        .transpose()
    }

    async fn set_global_reasoning_effort_override(
        &self,
        effort: ReasoningEffort,
    ) -> Result<(), ReasoningEffortOverrideStoreError> {
        retry_transient("global_reasoning_effort_override.set", || async {
            sqlx::query(
                "INSERT INTO global_reasoning_effort_overrides (singleton, reasoning_effort) \
                 VALUES (0, ?) \
                 ON CONFLICT(singleton) DO UPDATE SET \
                 reasoning_effort = excluded.reasoning_effort",
            )
            .bind(effort.as_str())
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
        .map_err(|err| map_effort_err(&err))
    }

    async fn clear_global_reasoning_effort_override(
        &self,
    ) -> Result<(), ReasoningEffortOverrideStoreError> {
        retry_transient("global_reasoning_effort_override.clear", || async {
            sqlx::query("DELETE FROM global_reasoning_effort_overrides WHERE singleton = 0")
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
        .map_err(|err| map_effort_err(&err))
    }
}
