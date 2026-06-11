//! Postgres-backed global model override store.

use std::str::FromStr;

use async_trait::async_trait;
use crabgent_core::{
    GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, ModelId, ModelOverrideStoreError,
    ReasoningEffort, ReasoningEffortOverrideStoreError,
};
use sqlx::{FromRow, PgPool};

use crate::retry::retry_transient;

/// Postgres implementation of [`GlobalModelOverrideStore`].
#[derive(Clone)]
pub struct PostgresGlobalOverrideStore {
    pool: PgPool,
}

impl PostgresGlobalOverrideStore {
    /// Create a global model override sub-store from a shared pool.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(FromRow)]
struct GlobalOverrideRow {
    model_id: String,
}

#[derive(FromRow)]
struct GlobalReasoningEffortOverrideRow {
    reasoning_effort: String,
}

fn map_err(err: &crabgent_store::StoreError) -> ModelOverrideStoreError {
    ModelOverrideStoreError::backend(err.to_string())
}

fn map_effort_err(err: &crabgent_store::StoreError) -> ReasoningEffortOverrideStoreError {
    ReasoningEffortOverrideStoreError::backend(err.to_string())
}

#[async_trait]
impl GlobalModelOverrideStore for PostgresGlobalOverrideStore {
    async fn get_global_model_override(&self) -> Result<Option<ModelId>, ModelOverrideStoreError> {
        let row = retry_transient("global_model_override.get", || async {
            sqlx::query_as::<_, GlobalOverrideRow>(
                "SELECT model_id FROM global_model_overrides WHERE singleton = 0",
            )
            .fetch_optional(&self.pool)
            .await
        })
        .await
        .map_err(|err| map_err(&err))?;
        Ok(row.map(|row| ModelId::new(row.model_id)))
    }

    async fn set_global_model_override(
        &self,
        model: &ModelId,
    ) -> Result<(), ModelOverrideStoreError> {
        retry_transient("global_model_override.set", || async {
            sqlx::query(
                "INSERT INTO global_model_overrides (singleton, model_id) VALUES (0, $1) \
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
impl GlobalReasoningEffortOverrideStore for PostgresGlobalOverrideStore {
    async fn get_global_reasoning_effort_override(
        &self,
    ) -> Result<Option<ReasoningEffort>, ReasoningEffortOverrideStoreError> {
        let row = retry_transient("global_reasoning_effort_override.get", || async {
            sqlx::query_as::<_, GlobalReasoningEffortOverrideRow>(
                "SELECT reasoning_effort FROM global_reasoning_effort_overrides WHERE singleton = 0",
            )
            .fetch_optional(&self.pool)
            .await
        })
        .await
        .map_err(|err| map_effort_err(&err))?;
        row.map(|row| {
            ReasoningEffort::from_str(&row.reasoning_effort)
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
                 VALUES (0, $1) \
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
