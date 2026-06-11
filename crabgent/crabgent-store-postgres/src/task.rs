//! Postgres task sub-store.

use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use crabgent_core::{Message, Owner, ReasoningEffort};
use crabgent_store::{
    Page, ParseTaskStatusError, SessionId, StoreError, Task, TaskId, TaskPauseCause,
    TaskResumeSpec, TaskStatus, TaskStore,
};
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::retry::retry_transient;

/// Task row projection. `transcript` is deliberately excluded so `get`/`list`
/// payloads stay lean; it is only touched by the dedicated
/// `save_transcript`/`load_transcript` statements.
const COLS: &str = "id, owner, name, prompt, status, output, error, created_at, updated_at, \
                    finished_at, parent_session_id, parent_task_id, context_mode, \
                    reasoning_effort_override, resume_spec, resume_count, pause_cause, \
                    paused_at";

/// Postgres implementation of `TaskStore`.
#[derive(Clone)]
pub struct PostgresTaskStore {
    pub(crate) pool: PgPool,
}

impl PostgresTaskStore {
    /// Create a task sub-store from a shared pool.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Borrow the shared sqlx pool.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[derive(FromRow)]
struct TaskRow {
    id: Uuid,
    owner: String,
    name: Option<String>,
    prompt: String,
    status: String,
    output: String,
    error: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    parent_session_id: Option<Uuid>,
    parent_task_id: Option<Uuid>,
    context_mode: Option<String>,
    reasoning_effort_override: Option<String>,
    resume_spec: Option<Json<serde_json::Value>>,
    resume_count: i64,
    pause_cause: Option<String>,
    paused_at: Option<DateTime<Utc>>,
}

impl TryFrom<TaskRow> for Task {
    type Error = StoreError;

    fn try_from(row: TaskRow) -> Result<Self, Self::Error> {
        let status = row
            .status
            .parse()
            .map_err(|err: ParseTaskStatusError| StoreError::invalid(err.to_string()))?;
        Ok(Self {
            id: TaskId::from_uuid(row.id),
            owner: Owner::new(row.owner),
            name: row.name,
            prompt: row.prompt,
            status,
            output: row.output,
            error: row.error,
            created_at: row.created_at,
            updated_at: row.updated_at,
            finished_at: row.finished_at,
            parent_session_id: row.parent_session_id.map(SessionId::from_uuid),
            parent_task_id: row.parent_task_id.map(TaskId::from_uuid),
            context_mode: row.context_mode,
            reasoning_effort_override: row
                .reasoning_effort_override
                .map(|effort| ReasoningEffort::from_str(&effort).map_err(StoreError::invalid))
                .transpose()?,
            // Lenient: a spec that no longer decodes (pre-1.0 shape break
            // on a surviving row) degrades to None so the resume scan can
            // finish the task deterministically instead of failing every
            // list_paused call.
            resume_spec: row
                .resume_spec
                .and_then(|Json(value)| serde_json::from_value::<TaskResumeSpec>(value).ok()),
            resume_count: u32::try_from(row.resume_count.max(0)).unwrap_or(u32::MAX),
            pause_cause: row
                .pause_cause
                .map(|cause| TaskPauseCause::from_str(&cause).map_err(StoreError::invalid))
                .transpose()?,
            paused_at: row.paused_at,
        })
    }
}

#[derive(FromRow)]
struct IdRow {
    id: Uuid,
}

#[derive(FromRow)]
struct TranscriptRow {
    transcript: Option<Json<Vec<Message>>>,
}

#[async_trait]
impl TaskStore for PostgresTaskStore {
    async fn insert(&self, task: &Task) -> Result<(), StoreError> {
        retry_transient("task.insert", || async {
            sqlx::query(
                "INSERT INTO tasks (id, owner, name, prompt, status, output, error, created_at, \
                 updated_at, finished_at, parent_session_id, parent_task_id, context_mode, \
                 reasoning_effort_override, resume_spec, resume_count, pause_cause, paused_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                 $17, $18)",
            )
            .bind(task.id.as_uuid())
            .bind(task.owner.as_str())
            .bind(task.name.as_deref())
            .bind(&task.prompt)
            .bind(task.status.as_str())
            .bind(&task.output)
            .bind(task.error.as_deref())
            .bind(task.created_at)
            .bind(task.updated_at)
            .bind(task.finished_at)
            .bind(task.parent_session_id.as_ref().map(SessionId::as_uuid))
            .bind(task.parent_task_id.as_ref().map(TaskId::as_uuid))
            .bind(task.context_mode.as_deref())
            .bind(task.reasoning_effort_override.map(ReasoningEffort::as_str))
            .bind(task.resume_spec.clone().map(Json))
            .bind(i64::from(task.resume_count))
            .bind(task.pause_cause.map(TaskPauseCause::as_str))
            .bind(task.paused_at)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn get(&self, id: &TaskId) -> Result<Option<Task>, StoreError> {
        let row = retry_transient("task.get", || async {
            sqlx::query_as::<_, TaskRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM tasks WHERE id = $1"
            )))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    async fn append_output(&self, id: &TaskId, chunk: &str) -> Result<(), StoreError> {
        let now = Utc::now();
        retry_transient("task.append_output", || async {
            sqlx::query("UPDATE tasks SET output = output || $1, updated_at = $2 WHERE id = $3")
                .bind(chunk)
                .bind(now)
                .bind(id.as_uuid())
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
    }

    async fn finish(
        &self,
        id: &TaskId,
        status: TaskStatus,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        if matches!(status, TaskStatus::Running | TaskStatus::Paused) {
            return Err(StoreError::invalid(
                "finish() requires Done or Failed status",
            ));
        }
        let now = Utc::now();
        retry_transient("task.finish", || async {
            sqlx::query(
                "UPDATE tasks SET status = $1, error = $2, finished_at = $3, updated_at = $4, \
                 pause_cause = NULL, paused_at = NULL \
                 WHERE id = $5",
            )
            .bind(status.as_str())
            .bind(error)
            .bind(now)
            .bind(now)
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn list_running(&self, page: Page) -> Result<Vec<Task>, StoreError> {
        let limit = i64::try_from(page.limit)
            .map_err(|err| StoreError::invalid(format!("page.limit out of range: {err}")))?;
        let offset = i64::try_from(page.offset)
            .map_err(|err| StoreError::invalid(format!("page.offset out of range: {err}")))?;
        let rows = retry_transient("task.list_running", || async {
            sqlx::query_as::<_, TaskRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM tasks WHERE status = 'running' \
                 ORDER BY created_at LIMIT $1 OFFSET $2"
            )))
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn list_by_owner(
        &self,
        owner: Option<&Owner>,
        page: Page,
    ) -> Result<Vec<Task>, StoreError> {
        let Some(owner) = owner else {
            return self.list_running(page).await;
        };
        let limit = i64::try_from(page.limit)
            .map_err(|err| StoreError::invalid(format!("page.limit out of range: {err}")))?;
        let offset = i64::try_from(page.offset)
            .map_err(|err| StoreError::invalid(format!("page.offset out of range: {err}")))?;
        let rows = retry_transient("task.list_by_owner", || async {
            sqlx::query_as::<_, TaskRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM tasks WHERE status = 'running' AND owner = $1 \
                 ORDER BY created_at LIMIT $2 OFFSET $3"
            )))
            .bind(owner.as_str())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn pause(&self, id: &TaskId, cause: TaskPauseCause) -> Result<bool, StoreError> {
        let now = Utc::now();
        let affected = retry_transient("task.pause", || async {
            sqlx::query(
                "UPDATE tasks SET status = 'paused', pause_cause = $1, paused_at = $2, \
                 updated_at = $2 WHERE id = $3 AND status = 'running'",
            )
            .bind(cause.as_str())
            .bind(now)
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected())
        })
        .await?;
        Ok(affected == 1)
    }

    async fn claim_for_resume(&self, id: &TaskId, max_resumes: u32) -> Result<bool, StoreError> {
        let now = Utc::now();
        let affected = retry_transient("task.claim_for_resume", || async {
            // Shutdown-cause pauses never count toward (or get blocked
            // by) the poison-task cap; see the trait docs.
            sqlx::query(
                "UPDATE tasks SET status = 'running', \
                 resume_count = resume_count + \
                     (CASE WHEN pause_cause = 'shutdown' THEN 0 ELSE 1 END), \
                 pause_cause = NULL, paused_at = NULL, updated_at = $1 \
                 WHERE id = $2 AND status = 'paused' \
                 AND (pause_cause = 'shutdown' OR resume_count < $3)",
            )
            .bind(now)
            .bind(id.as_uuid())
            .bind(i64::from(max_resumes))
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected())
        })
        .await?;
        Ok(affected == 1)
    }

    async fn list_paused(&self, page: Page) -> Result<Vec<Task>, StoreError> {
        let limit = i64::try_from(page.limit)
            .map_err(|err| StoreError::invalid(format!("page.limit out of range: {err}")))?;
        let offset = i64::try_from(page.offset)
            .map_err(|err| StoreError::invalid(format!("page.offset out of range: {err}")))?;
        let rows = retry_transient("task.list_paused", || async {
            sqlx::query_as::<_, TaskRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM tasks WHERE status = 'paused' \
                 ORDER BY created_at LIMIT $1 OFFSET $2"
            )))
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn pause_orphans(&self, stale_secs: i64) -> Result<Vec<TaskId>, StoreError> {
        let now = Utc::now();
        let cutoff = now - Duration::seconds(stale_secs);
        let rows = retry_transient("task.pause_orphans", || async {
            sqlx::query_as::<_, IdRow>(
                "UPDATE tasks SET status = 'paused', pause_cause = 'crash', paused_at = $1, \
                 updated_at = $1 WHERE status = 'running' AND updated_at < $2 RETURNING id",
            )
            .bind(now)
            .bind(cutoff)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| TaskId::from_uuid(row.id))
            .collect())
    }

    async fn list_children(&self, parent: &TaskId, page: Page) -> Result<Vec<Task>, StoreError> {
        let limit = i64::try_from(page.limit)
            .map_err(|err| StoreError::invalid(format!("page.limit out of range: {err}")))?;
        let offset = i64::try_from(page.offset)
            .map_err(|err| StoreError::invalid(format!("page.offset out of range: {err}")))?;
        let rows = retry_transient("task.list_children", || async {
            sqlx::query_as::<_, TaskRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM tasks WHERE parent_task_id = $1 \
                 ORDER BY created_at LIMIT $2 OFFSET $3"
            )))
            .bind(parent.as_uuid())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn save_transcript(&self, id: &TaskId, messages: &[Message]) -> Result<(), StoreError> {
        let transcript = Json(messages.to_vec());
        let now = Utc::now();
        retry_transient("task.save_transcript", || async {
            sqlx::query("UPDATE tasks SET transcript = $1, updated_at = $2 WHERE id = $3")
                .bind(&transcript)
                .bind(now)
                .bind(id.as_uuid())
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
    }

    async fn load_transcript(&self, id: &TaskId) -> Result<Option<Vec<Message>>, StoreError> {
        let row = retry_transient("task.load_transcript", || async {
            sqlx::query_as::<_, TranscriptRow>("SELECT transcript FROM tasks WHERE id = $1")
                .bind(id.as_uuid())
                .fetch_optional(&self.pool)
                .await
        })
        .await?;
        Ok(row
            .and_then(|r| r.transcript)
            .map(|Json(messages)| messages))
    }

    async fn recover_stuck(&self, timeout_secs: i64) -> Result<Vec<TaskId>, StoreError> {
        let cutoff = Utc::now() - Duration::seconds(timeout_secs);
        let now = Utc::now();
        let rows = retry_transient("task.recover_stuck", || async {
            sqlx::query_as::<_, IdRow>(
                "UPDATE tasks SET status = 'failed', error = 'stuck task recovered', \
                 finished_at = $1, updated_at = $1 \
                 WHERE status = 'running' AND updated_at < $2 RETURNING id",
            )
            .bind(now)
            .bind(cutoff)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| TaskId::from_uuid(row.id))
            .collect())
    }

    async fn cleanup_old(&self, days: i64) -> Result<u64, StoreError> {
        let cutoff = Utc::now() - Duration::days(days);
        retry_transient("task.cleanup_old", || async {
            sqlx::query("DELETE FROM tasks WHERE finished_at IS NOT NULL AND finished_at < $1")
                .bind(cutoff)
                .execute(&self.pool)
                .await
                .map(|result| result.rows_affected())
        })
        .await
    }
}
