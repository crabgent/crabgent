//! [`TaskNotifier`]: trait for delivering task-completion notifications.
//!
//! Implementors push the terminal task state to channels (Slack, Matrix,
//! email, ...) and report whether delivery succeeded. The executor calls
//! every registered notifier sequentially after the task reaches a
//! terminal state. A returning `Ok(false)` signals that delivery was
//! attempted but the channel rejected the message; `Err(_)` means the
//! attempt itself failed.

use async_trait::async_trait;
use crabgent_store::records::Task;

use crate::error::TaskError;

/// Delivers task-completion notifications.
#[async_trait]
pub trait TaskNotifier: Send + Sync {
    /// Send `message` for the terminal `task` state. Returns `Ok(true)`
    /// on successful delivery.
    async fn notify(&self, task: &Task, message: &str) -> Result<bool, TaskError>;
}

/// Notifier that drops every notification on the floor. Useful as a
/// default when callers do not need delivery, or as a sentinel in tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopNotifier;

#[async_trait]
impl TaskNotifier for NoopNotifier {
    async fn notify(&self, _task: &Task, _message: &str) -> Result<bool, TaskError> {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crabgent_store::records::TaskStatus;
    use crabgent_store::{Owner, TaskId};

    fn fixture() -> Task {
        let now = Utc::now();
        Task {
            resume_spec: None,
            resume_count: 0,
            pause_cause: None,
            paused_at: None,
            id: TaskId::new(),
            owner: Owner::new("u"),
            name: None,
            prompt: "p".into(),
            status: TaskStatus::Done,
            output: "out".into(),
            error: None,
            created_at: now,
            updated_at: now,
            finished_at: Some(now),
            parent_session_id: None,
            parent_task_id: None,
            context_mode: None,
            reasoning_effort_override: None,
        }
    }

    #[tokio::test]
    async fn noop_returns_ok_true() {
        let n = NoopNotifier;
        let task = fixture();
        let result = n.notify(&task, "hi").await.expect("test result");
        assert!(result);
    }

    #[tokio::test]
    async fn noop_clone_is_independent() {
        let a = NoopNotifier;
        let b = a;
        let task = fixture();
        assert!(a.notify(&task, "x").await.expect("test result"));
        assert!(b.notify(&task, "y").await.expect("test result"));
    }
}
