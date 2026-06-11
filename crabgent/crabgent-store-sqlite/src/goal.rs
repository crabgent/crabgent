//! SQLite-backed [`GoalStore`].

use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::{SqlitePool, SqliteRow};

use crabgent_core::Owner;
use crabgent_store::{
    GoalId, GoalStatus, GoalStore, Page, SessionId, StoreError, ThreadGoal, ThreadGoalUpdate,
};

use crate::retry::retry_transient;

const COLS: &str = "id, owner, session_id, objective, status, token_budget, tokens_used, \
                    time_used_seconds, created_at, updated_at";

#[derive(Clone)]
pub struct SqliteGoalStore {
    pool: SqlitePool,
}

impl SqliteGoalStore {
    pub(crate) const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn row_to_goal(row: &SqliteRow) -> Result<ThreadGoal, StoreError> {
    let id: String = row.try_get("id").map_err(StoreError::backend)?;
    let owner: String = row.try_get("owner").map_err(StoreError::backend)?;
    let session_id: String = row.try_get("session_id").map_err(StoreError::backend)?;
    let objective: String = row.try_get("objective").map_err(StoreError::backend)?;
    let status: String = row.try_get("status").map_err(StoreError::backend)?;
    let token_budget: Option<i64> = row.try_get("token_budget").map_err(StoreError::backend)?;
    let tokens_used: i64 = row.try_get("tokens_used").map_err(StoreError::backend)?;
    let time_used_seconds: i64 = row
        .try_get("time_used_seconds")
        .map_err(StoreError::backend)?;
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(StoreError::backend)?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at").map_err(StoreError::backend)?;

    Ok(ThreadGoal {
        id: GoalId::from_str(&id).map_err(StoreError::invalid)?,
        owner: Owner::new(owner),
        session: SessionId::from_str(&session_id).map_err(StoreError::invalid)?,
        objective,
        status: GoalStatus::from_str(&status).map_err(StoreError::invalid)?,
        token_budget,
        tokens_used,
        time_used_seconds,
        created_at,
        updated_at,
    })
}

#[async_trait]
impl GoalStore for SqliteGoalStore {
    async fn create(&self, goal: &ThreadGoal) -> Result<(), StoreError> {
        let id_s = goal.id.to_string();
        let owner = goal.owner.as_str().to_owned();
        let session_id = goal.session.to_string();
        let objective = goal.objective.clone();
        let status = goal.status.as_str();
        let token_budget = goal.token_budget;
        let tokens_used = goal.tokens_used;
        let time_used_seconds = goal.time_used_seconds;
        let created_at = goal.created_at;
        let updated_at = goal.updated_at;
        retry_transient("goal.create", || async {
            sqlx::query(
                "INSERT INTO thread_goals (id, owner, session_id, objective, status, \
                 token_budget, tokens_used, time_used_seconds, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id_s)
            .bind(&owner)
            .bind(&session_id)
            .bind(&objective)
            .bind(status)
            .bind(token_budget)
            .bind(tokens_used)
            .bind(time_used_seconds)
            .bind(created_at)
            .bind(updated_at)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
        .map_err(|e| match e {
            StoreError::Conflict(_) => {
                StoreError::Conflict(format!("session already has a goal: {session_id}"))
            }
            other => other,
        })
    }

    async fn get(&self, id: &GoalId) -> Result<Option<ThreadGoal>, StoreError> {
        let id_s = id.to_string();
        let row_opt = retry_transient("goal.get", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM thread_goals WHERE id = ?"
            )))
            .bind(&id_s)
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row_opt.as_ref().map(row_to_goal).transpose()
    }

    async fn get_for_session(&self, session: &SessionId) -> Result<Option<ThreadGoal>, StoreError> {
        let session_s = session.to_string();
        let row_opt = retry_transient("goal.get_for_session", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM thread_goals WHERE session_id = ?"
            )))
            .bind(&session_s)
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row_opt.as_ref().map(row_to_goal).transpose()
    }

    async fn update(&self, id: &GoalId, update: &ThreadGoalUpdate) -> Result<bool, StoreError> {
        let Some(existing) = self.get(id).await? else {
            return Ok(false);
        };
        let mut next = existing;
        update.apply_to(&mut next, Utc::now());
        let id_s = next.id.to_string();
        let objective = next.objective.clone();
        let status = next.status.as_str();
        let updated_at = next.updated_at;
        retry_transient("goal.update", || async {
            sqlx::query(
                "UPDATE thread_goals SET objective = ?, status = ?, updated_at = ? WHERE id = ?",
            )
            .bind(&objective)
            .bind(status)
            .bind(updated_at)
            .bind(&id_s)
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
        let id_s = id.to_string();
        let token_delta = token_delta.max(0);
        let time_delta = time_delta_seconds.max(0);
        let row_opt = retry_transient("goal.account_usage", || async {
            // SQLite evaluates every SET right-hand side against the original
            // row, so `tokens_used + ?` in the CASE compares the pre-update
            // total plus this delta against the budget, matching the in-memory
            // backend's "new tokens_used reaches budget" semantics.
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE thread_goals \
                 SET tokens_used = tokens_used + ?, \
                     time_used_seconds = time_used_seconds + ?, \
                     updated_at = ?, \
                     status = CASE \
                         WHEN status = 'active' AND token_budget IS NOT NULL \
                              AND token_budget > 0 AND tokens_used + ? >= token_budget \
                         THEN 'budget_limited' ELSE status END \
                 WHERE id = ? RETURNING {COLS}"
            )))
            .bind(token_delta)
            .bind(time_delta)
            .bind(at)
            .bind(token_delta)
            .bind(&id_s)
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row_opt.as_ref().map(row_to_goal).transpose()
    }

    async fn delete(&self, id: &GoalId) -> Result<bool, StoreError> {
        let id_s = id.to_string();
        let affected = retry_transient("goal.delete", || async {
            sqlx::query("DELETE FROM thread_goals WHERE id = ?")
                .bind(&id_s)
                .execute(&self.pool)
                .await
                .map(|r| r.rows_affected())
        })
        .await?;
        Ok(affected > 0)
    }

    async fn list_by_status(
        &self,
        status: GoalStatus,
        page: Page,
    ) -> Result<Vec<ThreadGoal>, StoreError> {
        let status_s = status.as_str();
        let limit = i64::try_from(page.limit).unwrap_or(i64::MAX);
        let offset = i64::try_from(page.offset).unwrap_or(i64::MAX);
        let rows = retry_transient("goal.list_by_status", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM thread_goals WHERE status = ? \
                 ORDER BY updated_at DESC LIMIT ? OFFSET ?"
            )))
            .bind(status_s)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.iter().map(row_to_goal).collect()
    }

    async fn resume_suspended(&self, at: DateTime<Utc>) -> Result<Vec<ThreadGoal>, StoreError> {
        let rows = retry_transient("goal.resume_suspended", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE thread_goals SET status = 'active', updated_at = ? \
                 WHERE status = 'suspended' RETURNING {COLS}"
            )))
            .bind(at)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.iter().map(row_to_goal).collect()
    }
}

#[cfg(test)]
mod tests;
