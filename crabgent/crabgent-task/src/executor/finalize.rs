//! Terminal handling for a drained task run: verdict persistence,
//! observer events, blocking-waiter signalling, and completion
//! notifiers. Split out of `spawn.rs` to keep that file under the
//! 500-line cap.

use std::sync::Arc;

use chrono::Utc;
use crabgent_core::ActivityTextSummary;
use crabgent_log::warn;
use crabgent_store::TaskId;
use crabgent_store::records::{Task, TaskPauseCause, TaskStatus};
use crabgent_store::traits::TaskStore;
use tokio::sync::oneshot;

use crate::drain::DrainOutcome;
use crate::notifier::TaskNotifier;
use crate::observer::{TaskActivityEvent, TaskActivityKind, TaskActivityMeta, notify_observers};

use super::spawn::DriveCtx;
use super::{CANCELLED_MESSAGE, TIMEOUT_MESSAGE};

/// Persist the pause transition. A `false` CAS result means the row left
/// `Running` through another path (e.g. a racing finish); a store error
/// leaves the row `Running` for boot-time orphan adoption. Both are
/// logged, never fatal.
pub(super) async fn pause_task<S>(store: &Arc<S>, task_id: &TaskId, cause: TaskPauseCause)
where
    S: TaskStore + 'static,
{
    match store.pause(task_id, cause).await {
        Ok(true) => {}
        Ok(false) => log_pause_skipped(task_id, cause),
        Err(error) => log_pause_failed(task_id, &error),
    }
}

fn log_pause_skipped(task_id: &TaskId, cause: TaskPauseCause) {
    warn!(
        task_id = %task_id,
        cause = cause.as_str(),
        "task executor: pause skipped; row is no longer running"
    );
}

fn log_pause_failed(task_id: &TaskId, error: &crabgent_store::StoreError) {
    warn!(
        task_id = %task_id,
        error = %error,
        "task executor: pause write failed; row converges via orphan adoption"
    );
}

pub(super) async fn observe_task_paused<S>(
    ctx: &DriveCtx<S>,
    task_id: &TaskId,
    cause: TaskPauseCause,
) where
    S: TaskStore + 'static,
{
    let meta = load_terminal_meta(ctx, task_id, TaskStatus::Paused).await;
    notify_observers(
        &ctx.observers,
        TaskActivityEvent {
            meta,
            kind: TaskActivityKind::Paused { cause },
        },
    )
    .await;
}

pub(super) async fn signal_completion<S>(ctx: &mut DriveCtx<S>, task_id: &TaskId)
where
    S: TaskStore + 'static,
{
    let Some(tx) = ctx.oneshot_tx.take() else {
        return;
    };
    signal_completion_tx(&ctx.store, task_id, tx).await;
}

async fn signal_completion_tx<S>(store: &Arc<S>, task_id: &TaskId, tx: oneshot::Sender<Task>)
where
    S: TaskStore + 'static,
{
    match store.get(task_id).await {
        Ok(task) => signal_loaded_task(task_id, tx, task),
        Err(error) => log_completion_load_failed(task_id, &error),
    }
}

fn signal_loaded_task(task_id: &TaskId, tx: oneshot::Sender<Task>, task: Option<Task>) {
    let Some(task) = task else {
        warn!(task_id = %task_id, "task executor: blocking waiter skipped; task missing");
        return;
    };
    send_blocking_waiter(task_id, tx, task);
}

fn send_blocking_waiter(task_id: &TaskId, tx: oneshot::Sender<Task>, task: Task) {
    if tx.send(task).is_err() {
        warn!(task_id = %task_id, "task executor: blocking waiter dropped before completion");
    }
}

fn log_completion_load_failed(task_id: &TaskId, error: &crabgent_store::StoreError) {
    warn!(
        task_id = %task_id,
        error = %error,
        "task executor: blocking waiter skipped; load failed"
    );
}

pub(super) async fn observe_task_started<S>(ctx: &DriveCtx<S>)
where
    S: TaskStore + 'static,
{
    notify_observers(
        &ctx.observers,
        TaskActivityEvent {
            meta: ctx.activity_meta.clone(),
            kind: TaskActivityKind::Started,
        },
    )
    .await;
}

pub(super) async fn observe_task_terminal<S>(
    ctx: &DriveCtx<S>,
    task_id: &TaskId,
    status: TaskStatus,
    error: Option<&str>,
) where
    S: TaskStore + 'static,
{
    let meta = load_terminal_meta(ctx, task_id, status).await;
    notify_observers(
        &ctx.observers,
        TaskActivityEvent {
            meta,
            kind: terminal_activity_kind(error),
        },
    )
    .await;
}

async fn load_terminal_meta<S>(
    ctx: &DriveCtx<S>,
    task_id: &TaskId,
    status: TaskStatus,
) -> TaskActivityMeta
where
    S: TaskStore + 'static,
{
    match ctx.store.get(task_id).await {
        Ok(Some(task)) => TaskActivityMeta::from_task(&task, ctx.activity_meta.run_id.clone()),
        Ok(None) => ctx
            .activity_meta
            .clone()
            .with_status(status, Some(Utc::now())),
        Err(error) => {
            warn!(
                task_id = %task_id,
                error = %error,
                "task observer: terminal metadata load failed"
            );
            ctx.activity_meta
                .clone()
                .with_status(status, Some(Utc::now()))
        }
    }
}

fn terminal_activity_kind(error: Option<&str>) -> TaskActivityKind {
    match error {
        None => TaskActivityKind::Completed,
        Some(CANCELLED_MESSAGE) => TaskActivityKind::Cancelled,
        Some(TIMEOUT_MESSAGE) => TaskActivityKind::TimedOut,
        Some(message) => TaskActivityKind::Failed {
            error: ActivityTextSummary::redacted(message),
        },
    }
}

pub(super) async fn finalize_task<S>(
    store: &Arc<S>,
    task_id: &TaskId,
    status: TaskStatus,
    error: Option<&str>,
) where
    S: TaskStore + 'static,
{
    if let Err(e) = store.finish(task_id, status, error).await {
        warn!(
            task_id = %task_id,
            error = %e,
            "task executor: finish failed; task may stay Running"
        );
    }
}

pub(super) async fn notify_completion<S>(
    store: &Arc<S>,
    task_id: &TaskId,
    notifiers: &[Arc<dyn TaskNotifier>],
    outcome: &DrainOutcome,
    error: Option<&str>,
) where
    S: TaskStore + 'static,
{
    if notifiers.is_empty() {
        return;
    }
    let Some(task) = load_notifier_task(store, task_id).await else {
        return;
    };
    let message = build_notification_message(outcome, error);
    notify_all(&task, notifiers, &message).await;
}

async fn load_notifier_task<S>(store: &Arc<S>, task_id: &TaskId) -> Option<Task>
where
    S: TaskStore + 'static,
{
    match store.get(task_id).await {
        Ok(task) => task,
        Err(e) => {
            warn!(
                task_id = %task_id,
                error = %e,
                "task executor: notifier dispatch skipped (load failed)"
            );
            None
        }
    }
}

async fn notify_all(task: &Task, notifiers: &[Arc<dyn TaskNotifier>], message: &str) {
    for notifier in notifiers {
        notify_one(task, notifier, message).await;
    }
}

async fn notify_one(task: &Task, notifier: &Arc<dyn TaskNotifier>, message: &str) {
    if let Err(e) = notifier.notify(task, message).await {
        warn!(
            task_id = %task.id,
            error = %e,
            "task executor: notifier failed"
        );
    }
}

pub(super) fn build_notification_message(outcome: &DrainOutcome, error: Option<&str>) -> String {
    if let Some(err) = error {
        return format!("task failed: {err}");
    }
    outcome.final_text.clone().unwrap_or_default()
}
