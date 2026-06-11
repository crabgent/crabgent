//! Typed observer events for background task progress.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::{ActivityEventSummary, ActivityTextSummary, ReasoningEffort, RunId};
use crabgent_log::warn;
use crabgent_store::records::{Task, TaskPauseCause, TaskStatus};
use crabgent_store::{Owner, SessionId, TaskId};

use crate::error::TaskError;

/// Receives compact progress events for task runs.
///
/// Implementations should enqueue quickly. Errors are logged by
/// [`TaskExecutor`](crate::TaskExecutor) and never fail the task.
#[async_trait]
pub trait TaskObserver: Send + Sync {
    async fn observe(&self, event: TaskActivityEvent) -> Result<(), TaskError>;
}

#[derive(Debug, Default)]
pub struct NoopTaskObserver;

#[async_trait]
impl TaskObserver for NoopTaskObserver {
    async fn observe(&self, _event: TaskActivityEvent) -> Result<(), TaskError> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TaskActivityEvent {
    pub meta: TaskActivityMeta,
    pub kind: TaskActivityKind,
}

#[derive(Debug, Clone)]
pub struct TaskActivityMeta {
    pub task_id: TaskId,
    pub owner: Owner,
    pub name: Option<ActivityTextSummary>,
    pub prompt: ActivityTextSummary,
    pub status: TaskStatus,
    pub run_id: RunId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration: Option<Duration>,
    pub parent_session_id: Option<SessionId>,
    pub parent_task_id: Option<TaskId>,
    pub context_mode: Option<String>,
    pub prompt_bytes: usize,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl TaskActivityMeta {
    #[must_use]
    pub fn from_task(task: &Task, run_id: RunId) -> Self {
        Self {
            task_id: task.id.clone(),
            owner: task.owner.clone(),
            name: task.name.as_deref().map(ActivityTextSummary::with_preview),
            prompt: ActivityTextSummary::with_preview(&task.prompt),
            status: task.status,
            run_id,
            created_at: task.created_at,
            updated_at: task.updated_at,
            finished_at: task.finished_at,
            duration: task_duration(task),
            parent_session_id: task.parent_session_id.clone(),
            parent_task_id: task.parent_task_id.clone(),
            context_mode: task.context_mode.clone(),
            prompt_bytes: task.prompt.len(),
            reasoning_effort: task.reasoning_effort_override,
        }
    }

    #[must_use]
    pub fn with_status(mut self, status: TaskStatus, finished_at: Option<DateTime<Utc>>) -> Self {
        self.status = status;
        self.finished_at = finished_at;
        self.duration = finished_at.and_then(|finished| {
            finished
                .signed_duration_since(self.created_at)
                .to_std()
                .ok()
        });
        self.updated_at = finished_at.unwrap_or_else(Utc::now);
        self
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TaskActivityKind {
    Started,
    Kernel(ActivityEventSummary),
    Completed,
    Failed {
        error: ActivityTextSummary,
    },
    Cancelled,
    TimedOut,
    /// The task was paused (not terminal): it stops running in this
    /// process and is eligible for restart-time resume.
    Paused {
        cause: TaskPauseCause,
    },
}

pub(crate) async fn notify_observers(
    observers: &[Arc<dyn TaskObserver>],
    event: TaskActivityEvent,
) {
    for observer in observers {
        notify_one(observer, event.clone()).await;
    }
}

async fn notify_one(observer: &Arc<dyn TaskObserver>, event: TaskActivityEvent) {
    if let Err(error) = observer.observe(event.clone()).await {
        warn!(
            task_id = %event.meta.task_id,
            error = %error,
            "task observer failed"
        );
    }
}

fn task_duration(task: &Task) -> Option<Duration> {
    task.finished_at.and_then(|finished| {
        finished
            .signed_duration_since(task.created_at)
            .to_std()
            .ok()
    })
}
