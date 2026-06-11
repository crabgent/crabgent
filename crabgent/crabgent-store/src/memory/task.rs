//! In-memory [`TaskStore`] backed by a `HashMap` behind a `Mutex`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use crabgent_core::Message;

use crate::error::StoreError;
use crate::ids::TaskId;
use crate::page::Page;
use crate::records::{Task, TaskPauseCause, TaskStatus};
use crate::traits::TaskStore;

#[derive(Default)]
pub struct MemoryTaskStore {
    inner: Mutex<HashMap<TaskId, Task>>,
    /// Transcripts live beside the task map so `get`/`list` payloads stay
    /// lean; lock order is always `inner` before `transcripts`.
    transcripts: Mutex<HashMap<TaskId, Vec<Message>>>,
}

impl MemoryTaskStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<TaskId, Task>>, StoreError> {
        self.inner
            .lock()
            .map_err(|e| StoreError::backend(format!("task mutex poisoned: {e}")))
    }

    fn lock_transcripts(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<TaskId, Vec<Message>>>, StoreError> {
        self.transcripts
            .lock()
            .map_err(|e| StoreError::backend(format!("task transcript mutex poisoned: {e}")))
    }

    fn list_by_status(&self, status: TaskStatus, page: Page) -> Result<Vec<Task>, StoreError> {
        let tasks = self.lock()?;
        let mut matching: Vec<&Task> = tasks.values().filter(|t| t.status == status).collect();
        matching.sort_by_key(|a| a.created_at);
        Ok(matching
            .into_iter()
            .skip(page.offset)
            .take(page.limit)
            .cloned()
            .collect())
    }
}

#[async_trait]
impl TaskStore for MemoryTaskStore {
    async fn insert(&self, task: &Task) -> Result<(), StoreError> {
        let mut tasks = self.lock()?;
        if tasks.contains_key(&task.id) {
            return Err(StoreError::Conflict(format!(
                "task already exists: {}",
                task.id
            )));
        }
        tasks.insert(task.id.clone(), task.clone());
        Ok(())
    }

    async fn get(&self, id: &TaskId) -> Result<Option<Task>, StoreError> {
        let tasks = self.lock()?;
        Ok(tasks.get(id).cloned())
    }

    async fn append_output(&self, id: &TaskId, chunk: &str) -> Result<(), StoreError> {
        let mut tasks = self.lock()?;
        if let Some(task) = tasks.get_mut(id) {
            task.output.push_str(chunk);
            task.updated_at = Utc::now();
        }
        Ok(())
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
        let mut tasks = self.lock()?;
        let Some(task) = tasks.get_mut(id) else {
            return Ok(());
        };
        let now = Utc::now();
        task.status = status;
        task.error = error.map(str::to_owned);
        task.finished_at = Some(now);
        task.updated_at = now;
        task.pause_cause = None;
        task.paused_at = None;
        Ok(())
    }

    async fn pause(&self, id: &TaskId, cause: TaskPauseCause) -> Result<bool, StoreError> {
        let mut tasks = self.lock()?;
        let Some(task) = tasks.get_mut(id) else {
            return Ok(false);
        };
        if !matches!(task.status, TaskStatus::Running) {
            return Ok(false);
        }
        let now = Utc::now();
        task.status = TaskStatus::Paused;
        task.pause_cause = Some(cause);
        task.paused_at = Some(now);
        task.updated_at = now;
        Ok(true)
    }

    async fn claim_for_resume(&self, id: &TaskId, max_resumes: u32) -> Result<bool, StoreError> {
        let mut tasks = self.lock()?;
        let Some(task) = tasks.get_mut(id) else {
            return Ok(false);
        };
        if !matches!(task.status, TaskStatus::Paused) {
            return Ok(false);
        }
        // Clean shutdown pauses are always claimable and never count
        // toward the poison-task cap; only Forced/Crash claims do.
        let counts_toward_cap = task.pause_cause != Some(TaskPauseCause::Shutdown);
        if counts_toward_cap {
            if task.resume_count >= max_resumes {
                return Ok(false);
            }
            task.resume_count += 1;
        }
        task.status = TaskStatus::Running;
        task.pause_cause = None;
        task.paused_at = None;
        task.updated_at = Utc::now();
        Ok(true)
    }

    async fn list_paused(&self, page: Page) -> Result<Vec<Task>, StoreError> {
        self.list_by_status(TaskStatus::Paused, page)
    }

    async fn pause_orphans(&self, stale_secs: i64) -> Result<Vec<TaskId>, StoreError> {
        let cutoff = Utc::now() - Duration::seconds(stale_secs);
        let mut tasks = self.lock()?;
        let orphans: Vec<TaskId> = tasks
            .values()
            .filter(|t| matches!(t.status, TaskStatus::Running) && t.updated_at < cutoff)
            .map(|t| t.id.clone())
            .collect();
        let now = Utc::now();
        for id in &orphans {
            if let Some(task) = tasks.get_mut(id) {
                task.status = TaskStatus::Paused;
                task.pause_cause = Some(TaskPauseCause::Crash);
                task.paused_at = Some(now);
                task.updated_at = now;
            }
        }
        Ok(orphans)
    }

    async fn list_children(&self, parent: &TaskId, page: Page) -> Result<Vec<Task>, StoreError> {
        let tasks = self.lock()?;
        let mut children: Vec<&Task> = tasks
            .values()
            .filter(|t| t.parent_task_id.as_ref() == Some(parent))
            .collect();
        children.sort_by_key(|a| a.created_at);
        Ok(children
            .into_iter()
            .skip(page.offset)
            .take(page.limit)
            .cloned()
            .collect())
    }

    async fn save_transcript(&self, id: &TaskId, messages: &[Message]) -> Result<(), StoreError> {
        let mut tasks = self.lock()?;
        let Some(task) = tasks.get_mut(id) else {
            return Ok(());
        };
        task.updated_at = Utc::now();
        let mut transcripts = self.lock_transcripts()?;
        transcripts.insert(id.clone(), messages.to_vec());
        Ok(())
    }

    async fn load_transcript(&self, id: &TaskId) -> Result<Option<Vec<Message>>, StoreError> {
        let transcripts = self.lock_transcripts()?;
        Ok(transcripts.get(id).cloned())
    }

    async fn list_running(&self, page: Page) -> Result<Vec<Task>, StoreError> {
        self.list_by_status(TaskStatus::Running, page)
    }

    async fn recover_stuck(&self, timeout_secs: i64) -> Result<Vec<TaskId>, StoreError> {
        let cutoff = Utc::now() - Duration::seconds(timeout_secs);
        let mut tasks = self.lock()?;
        let stuck: Vec<TaskId> = tasks
            .values()
            .filter(|t| matches!(t.status, TaskStatus::Running) && t.updated_at < cutoff)
            .map(|t| t.id.clone())
            .collect();
        for id in &stuck {
            if let Some(task) = tasks.get_mut(id) {
                let now = Utc::now();
                task.status = TaskStatus::Failed;
                task.error = Some("stuck task recovered".into());
                task.finished_at = Some(now);
                task.updated_at = now;
            }
        }
        Ok(stuck)
    }

    async fn cleanup_old(&self, days: i64) -> Result<u64, StoreError> {
        let cutoff = Utc::now() - Duration::days(days);
        let mut tasks = self.lock()?;
        let before = tasks.len();
        tasks.retain(|_, t| t.finished_at.is_none_or(|f| f >= cutoff));
        let mut transcripts = self.lock_transcripts()?;
        transcripts.retain(|id, _| tasks.contains_key(id));
        Ok((before - tasks.len()) as u64)
    }
}

#[cfg(test)]
#[path = "task_pause_tests.rs"]
mod pause_tests;
#[cfg(test)]
#[path = "task_tests.rs"]
mod tests;
