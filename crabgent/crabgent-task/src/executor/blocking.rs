use std::sync::Arc;
use std::time::Duration;

use crabgent_core::Kernel;
use crabgent_store::records::{Task, TaskStatus};
use crabgent_store::traits::TaskStore;
use tokio::sync::oneshot;
use tokio::time;

use crate::error::TaskError;
use crate::request::TaskRequest;

use super::{CANCELLED_MESSAGE, TIMEOUT_MESSAGE, TaskExecutor, spawn};

pub(super) async fn do_spawn_blocking<S>(
    exec: &TaskExecutor<S>,
    kernel: Arc<Kernel>,
    req: TaskRequest,
    timeout: Option<Duration>,
) -> Result<Task, TaskError>
where
    S: TaskStore + 'static,
{
    let (tx, rx) = oneshot::channel();
    let task_id = spawn::do_spawn_with_completion(exec, kernel, req, Some(tx)).await?;
    await_terminal_task(exec, &task_id, rx, timeout).await
}

async fn await_terminal_task<S>(
    exec: &TaskExecutor<S>,
    task_id: &crabgent_store::TaskId,
    rx: oneshot::Receiver<Task>,
    timeout: Option<Duration>,
) -> Result<Task, TaskError>
where
    S: TaskStore + 'static,
{
    let wait_for = timeout.unwrap_or(exec.timeout);
    match time::timeout(wait_for, rx).await {
        Ok(Ok(task)) => Ok(task),
        Ok(Err(_)) => load_task(exec, task_id).await,
        Err(_) => load_after_timeout(exec, task_id).await,
    }
}

async fn load_after_timeout<S>(
    exec: &TaskExecutor<S>,
    task_id: &crabgent_store::TaskId,
) -> Result<Task, TaskError>
where
    S: TaskStore + 'static,
{
    if let Some(task) = exec.store.get(task_id).await?
        && !matches!(task.status, TaskStatus::Running)
    {
        return Ok(task);
    }
    // cancel() reports whether a live token still existed; the terminal state
    // is reconciled below regardless, so the result is intentionally unused.
    let _live_token = exec.cancel(task_id).await;
    let task = wait_for_cancelled_task(exec, task_id).await?;
    if matches!(task.status, TaskStatus::Running)
        || task.error.as_deref() == Some(CANCELLED_MESSAGE)
    {
        exec.store
            .finish(task_id, TaskStatus::Failed, Some(TIMEOUT_MESSAGE))
            .await?;
        return load_task(exec, task_id).await;
    }
    Ok(task)
}

async fn wait_for_cancelled_task<S>(
    exec: &TaskExecutor<S>,
    task_id: &crabgent_store::TaskId,
) -> Result<Task, TaskError>
where
    S: TaskStore + 'static,
{
    let deadline = time::Instant::now() + exec.shutdown_grace + Duration::from_millis(50);
    loop {
        let task = load_task(exec, task_id).await?;
        if !matches!(task.status, TaskStatus::Running) {
            return Ok(task);
        }
        if time::Instant::now() >= deadline {
            return Ok(task);
        }
        time::sleep(Duration::from_millis(5)).await;
    }
}

async fn load_task<S>(
    exec: &TaskExecutor<S>,
    task_id: &crabgent_store::TaskId,
) -> Result<Task, TaskError>
where
    S: TaskStore + 'static,
{
    exec.store
        .get(task_id)
        .await?
        .ok_or_else(|| TaskError::executor(format!("task disappeared: {task_id}")))
}
