//! SQLite-backed [`TaskStore`].

use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::Row;
use sqlx::sqlite::{SqlitePool, SqliteRow};

use crabgent_core::{Message, Owner, ReasoningEffort};
use crabgent_store::{
    Page, ParseTaskStatusError, SessionId, StoreError, Task, TaskId, TaskPauseCause,
    TaskResumeSpec, TaskStatus, TaskStore,
};

use crate::retry::retry_transient;

/// Task row projection. `transcript` is deliberately excluded so `get`/`list`
/// payloads stay lean; it is only touched by the dedicated
/// `save_transcript`/`load_transcript` statements.
const COLS: &str = "id, owner, name, prompt, status, output, error, created_at, updated_at, \
                    finished_at, parent_session_id, parent_task_id, context_mode, \
                    reasoning_effort_override, resume_spec, resume_count, pause_cause, \
                    paused_at";

#[derive(Clone)]
pub struct SqliteTaskStore {
    pool: SqlitePool,
}

impl SqliteTaskStore {
    pub(crate) const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn parse_optional_id<F, I, E>(value: Option<String>, parse: F) -> Result<Option<I>, StoreError>
where
    F: FnOnce(&str) -> Result<I, E>,
    E: std::fmt::Display,
{
    value
        .map(|s| parse(&s).map_err(|e| StoreError::invalid(e.to_string())))
        .transpose()
}

fn row_to_task(row: &SqliteRow) -> Result<Task, StoreError> {
    let id: String = row.try_get("id").map_err(StoreError::backend)?;
    let owner: String = row.try_get("owner").map_err(StoreError::backend)?;
    let name: Option<String> = row.try_get("name").map_err(StoreError::backend)?;
    let prompt: String = row.try_get("prompt").map_err(StoreError::backend)?;
    let status_s: String = row.try_get("status").map_err(StoreError::backend)?;
    let output: String = row.try_get("output").map_err(StoreError::backend)?;
    let error: Option<String> = row.try_get("error").map_err(StoreError::backend)?;
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(StoreError::backend)?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at").map_err(StoreError::backend)?;
    let finished_at: Option<DateTime<Utc>> =
        row.try_get("finished_at").map_err(StoreError::backend)?;
    let parent_session_id: Option<String> = row
        .try_get("parent_session_id")
        .map_err(StoreError::backend)?;
    let parent_task_id: Option<String> =
        row.try_get("parent_task_id").map_err(StoreError::backend)?;
    let context_mode: Option<String> = row.try_get("context_mode").map_err(StoreError::backend)?;
    let reasoning_effort_override: Option<String> = row
        .try_get("reasoning_effort_override")
        .map_err(StoreError::backend)?;
    let resume_spec: Option<String> = row.try_get("resume_spec").map_err(StoreError::backend)?;
    let resume_count: i64 = row.try_get("resume_count").map_err(StoreError::backend)?;
    let pause_cause: Option<String> = row.try_get("pause_cause").map_err(StoreError::backend)?;
    let paused_at: Option<DateTime<Utc>> = row.try_get("paused_at").map_err(StoreError::backend)?;

    let status: TaskStatus = status_s
        .parse()
        .map_err(|e: ParseTaskStatusError| StoreError::invalid(e.to_string()))?;
    Ok(Task {
        id: TaskId::from_str(&id).map_err(StoreError::invalid)?,
        owner: Owner::new(owner),
        name,
        prompt,
        status,
        output,
        error,
        created_at,
        updated_at,
        finished_at,
        parent_session_id: parse_optional_id(parent_session_id, SessionId::from_str)?,
        parent_task_id: parse_optional_id(parent_task_id, TaskId::from_str)?,
        context_mode,
        reasoning_effort_override: reasoning_effort_override
            .map(|effort| ReasoningEffort::from_str(&effort).map_err(StoreError::invalid))
            .transpose()?,
        // Lenient: a spec that no longer decodes (pre-1.0 shape break on a
        // surviving row) degrades to None so the resume scan can finish the
        // task deterministically instead of failing every list_paused call.
        resume_spec: resume_spec.and_then(|raw| serde_json::from_str::<TaskResumeSpec>(&raw).ok()),
        resume_count: u32::try_from(resume_count.max(0)).unwrap_or(u32::MAX),
        pause_cause: pause_cause
            .map(|cause| TaskPauseCause::from_str(&cause).map_err(StoreError::invalid))
            .transpose()?,
        paused_at,
    })
}

#[async_trait]
impl TaskStore for SqliteTaskStore {
    async fn insert(&self, task: &Task) -> Result<(), StoreError> {
        let id_s = task.id.to_string();
        let owner_s = task.owner.as_str().to_owned();
        let name = task.name.clone();
        let prompt = task.prompt.clone();
        let status = task.status.as_str().to_owned();
        let output = task.output.clone();
        let error = task.error.clone();
        let created_at = task.created_at;
        let updated_at = task.updated_at;
        let finished_at = task.finished_at;
        let parent_session = task.parent_session_id.as_ref().map(ToString::to_string);
        let parent_task = task.parent_task_id.as_ref().map(ToString::to_string);
        let context_mode = task.context_mode.clone();
        let reasoning_effort_override = task.reasoning_effort_override.map(ReasoningEffort::as_str);
        let resume_spec = task
            .resume_spec
            .as_ref()
            .map(|spec| serde_json::to_string(spec).map_err(StoreError::invalid))
            .transpose()?;
        let resume_count = i64::from(task.resume_count);
        let pause_cause = task.pause_cause.map(TaskPauseCause::as_str);
        let paused_at = task.paused_at;
        retry_transient("task.insert", || async {
            sqlx::query(
                "INSERT INTO tasks (id, owner, name, prompt, status, output, error, \
                 created_at, updated_at, finished_at, parent_session_id, parent_task_id, \
                 context_mode, reasoning_effort_override, resume_spec, resume_count, \
                 pause_cause, paused_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id_s)
            .bind(&owner_s)
            .bind(name.as_deref())
            .bind(&prompt)
            .bind(&status)
            .bind(&output)
            .bind(error.as_deref())
            .bind(created_at)
            .bind(updated_at)
            .bind(finished_at)
            .bind(parent_session.as_deref())
            .bind(parent_task.as_deref())
            .bind(context_mode.as_deref())
            .bind(reasoning_effort_override)
            .bind(resume_spec.as_deref())
            .bind(resume_count)
            .bind(pause_cause)
            .bind(paused_at)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
        .map_err(|e| match e {
            StoreError::Conflict(_) => StoreError::Conflict(format!("task already exists: {id_s}")),
            other => other,
        })
    }

    async fn get(&self, id: &TaskId) -> Result<Option<Task>, StoreError> {
        let id_s = id.to_string();
        let row_opt = retry_transient("task.get", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM tasks WHERE id = ?"
            )))
            .bind(&id_s)
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row_opt.as_ref().map(row_to_task).transpose()
    }

    async fn append_output(&self, id: &TaskId, chunk: &str) -> Result<(), StoreError> {
        let id_s = id.to_string();
        let chunk_s = chunk.to_owned();
        let now = Utc::now();
        retry_transient("task.append_output", || async {
            sqlx::query("UPDATE tasks SET output = output || ?, updated_at = ? WHERE id = ?")
                .bind(&chunk_s)
                .bind(now)
                .bind(&id_s)
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
        let id_s = id.to_string();
        let status_s = status.as_str().to_owned();
        let error_s = error.map(str::to_owned);
        let now = Utc::now();
        retry_transient("task.finish", || async {
            sqlx::query(
                "UPDATE tasks SET status = ?, error = ?, finished_at = ?, updated_at = ?, \
                 pause_cause = NULL, paused_at = NULL \
                 WHERE id = ?",
            )
            .bind(&status_s)
            .bind(error_s.as_deref())
            .bind(now)
            .bind(now)
            .bind(&id_s)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn list_running(&self, page: Page) -> Result<Vec<Task>, StoreError> {
        let limit = i64::try_from(page.limit).unwrap_or(i64::MAX);
        let offset = i64::try_from(page.offset).unwrap_or(i64::MAX);
        let rows = retry_transient("task.list_running", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM tasks WHERE status = 'running' \
                 ORDER BY created_at LIMIT ? OFFSET ?"
            )))
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.iter().map(row_to_task).collect()
    }

    async fn list_by_owner(
        &self,
        owner: Option<&Owner>,
        page: Page,
    ) -> Result<Vec<Task>, StoreError> {
        let Some(owner) = owner else {
            return self.list_running(page).await;
        };
        let owner_s = owner.as_str().to_owned();
        let limit = i64::try_from(page.limit).unwrap_or(i64::MAX);
        let offset = i64::try_from(page.offset).unwrap_or(i64::MAX);
        let rows = retry_transient("task.list_by_owner", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM tasks WHERE status = 'running' AND owner = ? \
                 ORDER BY created_at LIMIT ? OFFSET ?"
            )))
            .bind(&owner_s)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.iter().map(row_to_task).collect()
    }

    async fn pause(&self, id: &TaskId, cause: TaskPauseCause) -> Result<bool, StoreError> {
        let id_s = id.to_string();
        let cause_s = cause.as_str();
        let now = Utc::now();
        let affected = retry_transient("task.pause", || async {
            sqlx::query(
                "UPDATE tasks SET status = 'paused', pause_cause = ?, paused_at = ?, \
                 updated_at = ? WHERE id = ? AND status = 'running'",
            )
            .bind(cause_s)
            .bind(now)
            .bind(now)
            .bind(&id_s)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected())
        })
        .await?;
        Ok(affected == 1)
    }

    async fn claim_for_resume(&self, id: &TaskId, max_resumes: u32) -> Result<bool, StoreError> {
        let id_s = id.to_string();
        let max = i64::from(max_resumes);
        let now = Utc::now();
        let affected = retry_transient("task.claim_for_resume", || async {
            // Shutdown-cause pauses never count toward (or get blocked
            // by) the poison-task cap; see the trait docs.
            sqlx::query(
                "UPDATE tasks SET status = 'running', \
                 resume_count = resume_count + \
                     (CASE WHEN pause_cause = 'shutdown' THEN 0 ELSE 1 END), \
                 pause_cause = NULL, paused_at = NULL, updated_at = ? \
                 WHERE id = ? AND status = 'paused' \
                 AND (pause_cause = 'shutdown' OR resume_count < ?)",
            )
            .bind(now)
            .bind(&id_s)
            .bind(max)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected())
        })
        .await?;
        Ok(affected == 1)
    }

    async fn list_paused(&self, page: Page) -> Result<Vec<Task>, StoreError> {
        let limit = i64::try_from(page.limit).unwrap_or(i64::MAX);
        let offset = i64::try_from(page.offset).unwrap_or(i64::MAX);
        let rows = retry_transient("task.list_paused", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM tasks WHERE status = 'paused' \
                 ORDER BY created_at LIMIT ? OFFSET ?"
            )))
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.iter().map(row_to_task).collect()
    }

    async fn pause_orphans(&self, stale_secs: i64) -> Result<Vec<TaskId>, StoreError> {
        let now = Utc::now();
        let cutoff = now - Duration::seconds(stale_secs);
        let rows = retry_transient("task.pause_orphans", || async {
            sqlx::query(
                "UPDATE tasks SET status = 'paused', pause_cause = 'crash', paused_at = ?, \
                 updated_at = ? WHERE status = 'running' AND updated_at < ? RETURNING id",
            )
            .bind(now)
            .bind(now)
            .bind(cutoff)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.iter()
            .map(|row| {
                let id: String = row.try_get("id").map_err(StoreError::backend)?;
                TaskId::from_str(&id).map_err(StoreError::invalid)
            })
            .collect()
    }

    async fn list_children(&self, parent: &TaskId, page: Page) -> Result<Vec<Task>, StoreError> {
        let parent_s = parent.to_string();
        let limit = i64::try_from(page.limit).unwrap_or(i64::MAX);
        let offset = i64::try_from(page.offset).unwrap_or(i64::MAX);
        let rows = retry_transient("task.list_children", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM tasks WHERE parent_task_id = ? \
                 ORDER BY created_at LIMIT ? OFFSET ?"
            )))
            .bind(&parent_s)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.iter().map(row_to_task).collect()
    }

    async fn save_transcript(&self, id: &TaskId, messages: &[Message]) -> Result<(), StoreError> {
        let id_s = id.to_string();
        let transcript = serde_json::to_string(messages).map_err(StoreError::invalid)?;
        let now = Utc::now();
        retry_transient("task.save_transcript", || async {
            sqlx::query("UPDATE tasks SET transcript = ?, updated_at = ? WHERE id = ?")
                .bind(&transcript)
                .bind(now)
                .bind(&id_s)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
    }

    async fn load_transcript(&self, id: &TaskId) -> Result<Option<Vec<Message>>, StoreError> {
        let id_s = id.to_string();
        let row_opt = retry_transient("task.load_transcript", || async {
            sqlx::query("SELECT transcript FROM tasks WHERE id = ?")
                .bind(&id_s)
                .fetch_optional(&self.pool)
                .await
        })
        .await?;
        let Some(row) = row_opt else {
            return Ok(None);
        };
        let raw: Option<String> = row.try_get("transcript").map_err(StoreError::backend)?;
        raw.map(|json| serde_json::from_str::<Vec<Message>>(&json).map_err(StoreError::invalid))
            .transpose()
    }

    async fn recover_stuck(&self, timeout_secs: i64) -> Result<Vec<TaskId>, StoreError> {
        let now = Utc::now();
        let cutoff = now - Duration::seconds(timeout_secs);
        let rows = retry_transient("task.recover_stuck", || async {
            sqlx::query(
                "UPDATE tasks SET status = 'failed', error = 'stuck task recovered', \
                 finished_at = ?, updated_at = ? \
                 WHERE status = 'running' AND updated_at < ? RETURNING id",
            )
            .bind(now)
            .bind(now)
            .bind(cutoff)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.iter()
            .map(|row| {
                let id: String = row.try_get("id").map_err(StoreError::backend)?;
                TaskId::from_str(&id).map_err(StoreError::invalid)
            })
            .collect()
    }

    async fn cleanup_old(&self, days: i64) -> Result<u64, StoreError> {
        let cutoff = Utc::now() - Duration::days(days);
        let affected = retry_transient("task.cleanup_old", || async {
            sqlx::query("DELETE FROM tasks WHERE finished_at IS NOT NULL AND finished_at < ?")
                .bind(cutoff)
                .execute(&self.pool)
                .await
                .map(|r| r.rows_affected())
        })
        .await?;
        Ok(affected)
    }
}

#[cfg(test)]
#[path = "task_pause_tests.rs"]
mod pause_tests;
#[cfg(test)]
#[path = "task_tests.rs"]
mod tests;
