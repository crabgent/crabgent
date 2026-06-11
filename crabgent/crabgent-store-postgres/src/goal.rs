//! Postgres goal sub-store.

use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::Owner;
use crabgent_store::{
    GoalId, GoalStatus, GoalStore, Page, SessionId, StoreError, ThreadGoal, ThreadGoalUpdate,
};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::retry::retry_transient;

const COLS: &str = "id, owner, session_id, objective, status, token_budget, tokens_used, \
                    time_used_seconds, created_at, updated_at";

/// Postgres implementation of [`GoalStore`].
#[derive(Clone)]
pub struct PostgresGoalStore {
    pool: PgPool,
}

impl PostgresGoalStore {
    /// Create a goal sub-store from a shared pool.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(FromRow)]
struct ThreadGoalRow {
    id: Uuid,
    owner: String,
    session_id: Uuid,
    objective: String,
    status: String,
    token_budget: Option<i64>,
    tokens_used: i64,
    time_used_seconds: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<ThreadGoalRow> for ThreadGoal {
    type Error = StoreError;

    fn try_from(row: ThreadGoalRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: GoalId::from_uuid(row.id),
            owner: Owner::new(row.owner),
            session: SessionId::from_uuid(row.session_id),
            objective: row.objective,
            status: GoalStatus::from_str(&row.status).map_err(StoreError::invalid)?,
            token_budget: row.token_budget,
            tokens_used: row.tokens_used,
            time_used_seconds: row.time_used_seconds,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[async_trait]
impl GoalStore for PostgresGoalStore {
    async fn create(&self, goal: &ThreadGoal) -> Result<(), StoreError> {
        let status = goal.status.as_str();
        retry_transient("goal.create", || async {
            sqlx::query(
                "INSERT INTO thread_goals (id, owner, session_id, objective, status, \
                 token_budget, tokens_used, time_used_seconds, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(goal.id.as_uuid())
            .bind(goal.owner.as_str())
            .bind(goal.session.as_uuid())
            .bind(&goal.objective)
            .bind(status)
            .bind(goal.token_budget)
            .bind(goal.tokens_used)
            .bind(goal.time_used_seconds)
            .bind(goal.created_at)
            .bind(goal.updated_at)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
        .map_err(|e| match e {
            StoreError::Conflict(_) => {
                StoreError::Conflict(format!("session already has a goal: {}", goal.session))
            }
            other => other,
        })
    }

    async fn get(&self, id: &GoalId) -> Result<Option<ThreadGoal>, StoreError> {
        let row = retry_transient("goal.get", || async {
            sqlx::query_as::<_, ThreadGoalRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM thread_goals WHERE id = $1"
            )))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    async fn get_for_session(&self, session: &SessionId) -> Result<Option<ThreadGoal>, StoreError> {
        let row = retry_transient("goal.get_for_session", || async {
            sqlx::query_as::<_, ThreadGoalRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM thread_goals WHERE session_id = $1"
            )))
            .bind(session.as_uuid())
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    async fn update(&self, id: &GoalId, update: &ThreadGoalUpdate) -> Result<bool, StoreError> {
        let Some(existing) = self.get(id).await? else {
            return Ok(false);
        };
        let mut next = existing;
        update.apply_to(&mut next, Utc::now());
        let status = next.status.as_str();
        retry_transient("goal.update", || async {
            sqlx::query(
                "UPDATE thread_goals SET objective = $1, status = $2, updated_at = $3 WHERE id = $4",
            )
            .bind(&next.objective)
            .bind(status)
            .bind(next.updated_at)
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await?;
        Ok(true)
    }

    async fn account_usage(
        &self,
        id: &GoalId,
        token_delta: i64,
        time_delta_seconds: i64,
        at: DateTime<Utc>,
    ) -> Result<Option<ThreadGoal>, StoreError> {
        let token_delta = token_delta.max(0);
        let time_delta = time_delta_seconds.max(0);
        let row = retry_transient("goal.account_usage", || async {
            // Postgres evaluates SET right-hand sides against the pre-update
            // row, so `tokens_used + $1` in the CASE compares the prior total
            // plus this delta against the budget, matching the in-memory and
            // sqlite backends' "new tokens_used reaches budget" semantics.
            sqlx::query_as::<_, ThreadGoalRow>(sqlx::AssertSqlSafe(format!(
                "UPDATE thread_goals \
                 SET tokens_used = tokens_used + $1, \
                     time_used_seconds = time_used_seconds + $2, \
                     updated_at = $3, \
                     status = CASE \
                         WHEN status = 'active' AND token_budget IS NOT NULL \
                              AND token_budget > 0 AND tokens_used + $1 >= token_budget \
                         THEN 'budget_limited' ELSE status END \
                 WHERE id = $4 RETURNING {COLS}"
            )))
            .bind(token_delta)
            .bind(time_delta)
            .bind(at)
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    async fn delete(&self, id: &GoalId) -> Result<bool, StoreError> {
        let affected = retry_transient("goal.delete", || async {
            sqlx::query("DELETE FROM thread_goals WHERE id = $1")
                .bind(id.as_uuid())
                .execute(&self.pool)
                .await
                .map(|result| result.rows_affected())
        })
        .await?;
        Ok(affected > 0)
    }

    async fn list_by_status(
        &self,
        status: GoalStatus,
        page: Page,
    ) -> Result<Vec<ThreadGoal>, StoreError> {
        let limit = i64::try_from(page.limit)
            .map_err(|err| StoreError::invalid(format!("page.limit out of range: {err}")))?;
        let offset = i64::try_from(page.offset)
            .map_err(|err| StoreError::invalid(format!("page.offset out of range: {err}")))?;
        let rows = retry_transient("goal.list_by_status", || async {
            sqlx::query_as::<_, ThreadGoalRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM thread_goals WHERE status = $1 \
                 ORDER BY updated_at DESC LIMIT $2 OFFSET $3"
            )))
            .bind(status.as_str())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn resume_suspended(&self, at: DateTime<Utc>) -> Result<Vec<ThreadGoal>, StoreError> {
        let rows = retry_transient("goal.resume_suspended", || async {
            sqlx::query_as::<_, ThreadGoalRow>(sqlx::AssertSqlSafe(format!(
                "UPDATE thread_goals SET status = 'active', updated_at = $1 \
                 WHERE status = 'suspended' RETURNING {COLS}"
            )))
            .bind(at)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}
