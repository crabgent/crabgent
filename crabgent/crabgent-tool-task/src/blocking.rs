//! Blocking and cancellation wait helpers for [`crate::TaskTool`].

use std::time::Duration;

use crabgent_store::{Task, TaskId, TaskStatus, TaskStore};

use crate::tool::store_unavailable;

pub fn timeout(timeout_secs: Option<u64>) -> Option<Duration> {
    timeout_secs.map(Duration::from_secs)
}

pub async fn wait_after_cancel_for<S>(
    store: &S,
    id: &TaskId,
    wait_for: Duration,
) -> Result<Task, crabgent_core::ToolError>
where
    S: TaskStore + ?Sized,
{
    let deadline = tokio::time::Instant::now() + wait_for;
    loop {
        let task = store
            .get(id)
            .await
            .map_err(|err| store_unavailable("cancel", &err))?
            .ok_or_else(|| crabgent_core::ToolError::NotFound(format!("task {id}")))?;
        if !matches!(task.status, TaskStatus::Running) || tokio::time::Instant::now() >= deadline {
            return Ok(task);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
