use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::Utc;
use crabgent_core::message::Message;
use crabgent_core::run::RunRequest;
use crabgent_core::run_id::RunId;
use crabgent_core::types::ToolCall;
use crabgent_core::{CancelReason, ContentBlock, Kernel, ToolAccess};
use crabgent_store::TaskId;
use crabgent_store::records::{Task, TaskPauseCause, TaskResumeSpec, TaskStatus};
use crabgent_store::traits::TaskStore;
use tokio::sync::{Mutex, OwnedSemaphorePermit, oneshot};
use tokio_util::sync::CancellationToken;

use crate::TASK_ID_ATTR;
use crate::drain::{DrainOutcome, drain_stream_observed};
use crate::error::TaskError;
use crate::notifier::TaskNotifier;
use crate::observer::{TaskActivityMeta, TaskObserver};
use crate::request::TaskRequest;

use super::finalize::{
    finalize_task, notify_completion, observe_task_paused, observe_task_started,
    observe_task_terminal, pause_task, signal_completion,
};
use super::{
    CANCELLED_MESSAGE, TIMEOUT_MESSAGE, TaskExecutor, TaskHandles, drain_finished_locked,
    drain_timeout,
};

/// Error recorded when a kernel stream closes cleanly without emitting
/// `Event::Final` and without a mid-flight error. Treating this as a failure is
/// correct only because the kernel always emits `Event::Final` before closing
/// the channel on a successful turn; a silent close therefore means the run was
/// dropped, not that an output-less turn succeeded.
const NO_FINAL_EVENT_MESSAGE: &str = "run ended without final event";

/// Internal handle bundling everything a spawned drain needs.
pub(super) struct DriveCtx<S: TaskStore> {
    pub(crate) kernel: Arc<Kernel>,
    pub(crate) store: Arc<S>,
    pub(crate) notifiers: Vec<Arc<dyn TaskNotifier>>,
    pub(crate) observers: Vec<Arc<dyn TaskObserver>>,
    pub(crate) activity_meta: TaskActivityMeta,
    pub(crate) timeout: Duration,
    pub(crate) shutdown_grace: Duration,
    pub(crate) progress_debounce: Duration,
    pub(crate) output_debounce_bytes: usize,
    pub(crate) cancel: CancellationToken,
    /// Per-task pause child (of the executor pause root). Its fired state
    /// at classification time means the cancel that ended the run was a
    /// shutdown force-pause, not a plain failure.
    pub(crate) pause: CancellationToken,
    /// Shared cancel-reason attribution cell, also installed on the
    /// `RunRequest` and in the executor's handle map.
    pub(crate) reason: Arc<OnceLock<CancelReason>>,
    pub(crate) cancel_handles: Arc<Mutex<HashMap<TaskId, TaskHandles>>>,
    pub(crate) oneshot_tx: Option<oneshot::Sender<Task>>,
    pub(crate) tool_access: ToolAccess,
}

pub(super) async fn do_spawn<S>(
    exec: &TaskExecutor<S>,
    kernel: Arc<Kernel>,
    req: TaskRequest,
) -> Result<TaskId, TaskError>
where
    S: TaskStore + 'static,
{
    do_spawn_with_completion(exec, kernel, req, None).await
}

pub(super) async fn do_spawn_with_completion<S>(
    exec: &TaskExecutor<S>,
    kernel: Arc<Kernel>,
    req: TaskRequest,
    oneshot_tx: Option<oneshot::Sender<Task>>,
) -> Result<TaskId, TaskError>
where
    S: TaskStore + 'static,
{
    let permit = acquire_spawn_permit(exec).await?;
    let task = build_task_record(&req);
    exec.store.insert(&task).await?;
    Ok(launch_run(exec, kernel, &req, task, permit, oneshot_tx).await)
}

/// Acquire a concurrency permit, rejecting once shutdown or the pause
/// window has started.
pub(super) async fn acquire_spawn_permit<S>(
    exec: &TaskExecutor<S>,
) -> Result<OwnedSemaphorePermit, TaskError>
where
    S: TaskStore + 'static,
{
    if exec.max_parallel == 0 {
        return Err(TaskError::executor(
            "task executor max_parallel must be at least 1",
        ));
    }
    tokio::select! {
        biased;
        () = exec.cancel.cancelled() => {
            Err(TaskError::executor("task executor is shutting down"))
        }
        () = exec.pause_root.cancelled() => {
            Err(TaskError::executor("task executor is pausing for shutdown"))
        }
        result = Arc::clone(&exec.semaphore).acquire_owned() => {
            result.map_err(|_closed| TaskError::executor("task executor concurrency limiter closed"))
        }
    }
}

/// Wire up tokens, handles, and the drive task for an already-persisted
/// task record. Shared between fresh spawns and restart-time resume
/// (which claims an existing record instead of inserting one).
pub(super) async fn launch_run<S>(
    exec: &TaskExecutor<S>,
    kernel: Arc<Kernel>,
    req: &TaskRequest,
    task: Task,
    permit: OwnedSemaphorePermit,
    oneshot_tx: Option<oneshot::Sender<Task>>,
) -> TaskId
where
    S: TaskStore + 'static,
{
    let task_id = task.id.clone();
    let pause = exec.pause_root.child_token();
    let reason: Arc<OnceLock<CancelReason>> = Arc::new(OnceLock::new());
    let run_req = build_run_request(req, &task, pause.clone(), Arc::clone(&reason));
    let activity_meta = TaskActivityMeta::from_task(&task, run_req.run_id.clone());

    let child_token = exec.cancel.child_token();
    exec.cancel_handles.lock().await.insert(
        task_id.clone(),
        TaskHandles {
            cancel: child_token.clone(),
            reason: Arc::clone(&reason),
        },
    );

    let ctx = DriveCtx {
        kernel,
        store: Arc::clone(&exec.store),
        notifiers: exec.notifiers.clone(),
        observers: exec.observers.clone(),
        activity_meta,
        timeout: exec.timeout,
        shutdown_grace: exec.shutdown_grace,
        progress_debounce: exec.progress_debounce,
        output_debounce_bytes: exec.output_debounce_bytes,
        cancel: child_token,
        pause,
        reason,
        cancel_handles: Arc::clone(&exec.cancel_handles),
        oneshot_tx,
        tool_access: req.tool_access.clone(),
    };
    let mut tasks = exec.tasks.lock().await;
    drain_finished_locked(&mut tasks);
    let task_id_for_join = task_id.clone();
    tasks.spawn(async move {
        let _permit = permit;
        drive_one_task(ctx, task_id_for_join.clone(), run_req).await;
        task_id_for_join
    });
    task_id
}

pub(super) fn build_task_record(req: &TaskRequest) -> Task {
    let now = Utc::now();
    Task {
        id: TaskId::new(),
        owner: req.owner.clone(),
        name: req.name.clone(),
        prompt: req.prompt.clone(),
        status: TaskStatus::Running,
        output: String::new(),
        error: None,
        created_at: now,
        updated_at: now,
        finished_at: None,
        parent_session_id: req.parent_session_id.clone(),
        parent_task_id: req.parent_task_id.clone(),
        context_mode: req.context_mode.clone(),
        reasoning_effort_override: req.reasoning_effort,
        resume_spec: Some(build_resume_spec(req)),
        resume_count: 0,
        pause_cause: None,
        paused_at: None,
    }
}

/// Snapshot the run parameters needed to rebuild this task's
/// `RunRequest` after a restart. The original `TaskRequest.subject` is
/// captured (pre task-id stamp; the stamp is reapplied on respawn).
fn build_resume_spec(req: &TaskRequest) -> TaskResumeSpec {
    TaskResumeSpec {
        subject_id: req.subject.id().to_owned(),
        subject_attrs: req.subject.attrs().clone(),
        model: req.model.clone().into(),
        explicit_model: req.explicit_model.clone().map(Into::into),
        session_model_override: req
            .session_model_override
            .as_ref()
            .map(|model| model.as_str().to_owned()),
        reasoning_effort: req.reasoning_effort,
        system_prompt: req.system_prompt.clone(),
        max_turns: req.max_turns,
        tool_access: req.tool_access.clone(),
    }
}

pub(super) fn build_run_request(
    req: &TaskRequest,
    task: &Task,
    pause: CancellationToken,
    cancel_reason: Arc<OnceLock<CancelReason>>,
) -> RunRequest {
    let mut messages = filter_messages_for_tool_access(req.messages.clone(), &req.tool_access);
    if messages.is_empty() {
        messages.push(Message::User {
            content: vec![ContentBlock::Text {
                text: req.prompt.clone(),
            }],
            timestamp: None,
        });
    }
    let subject = req
        .subject
        .clone()
        .with_attr(TASK_ID_ATTR, task.id.to_string());
    RunRequest {
        pause: Some(pause),
        run_id: RunId::new(),
        subject,
        model: req.model.clone(),
        explicit_model: req.explicit_model.clone(),
        session_model_override: req.session_model_override.clone(),
        fallbacks: Vec::new(),
        messages,
        system_prompt: req.system_prompt.clone(),
        max_turns: req.max_turns,
        temperature: None,
        max_tokens: None,
        cancel_reason: Some(cancel_reason),
        reasoning_effort: req.reasoning_effort,
        web_search: crabgent_core::types::WebSearchConfig::default(),
    }
}

pub(super) fn filter_messages_for_tool_access(
    messages: Vec<Message>,
    access: &ToolAccess,
) -> Vec<Message> {
    if matches!(access, ToolAccess::All) {
        return messages;
    }
    let removed_call_ids = removed_tool_call_ids(&messages, access);
    messages
        .into_iter()
        .filter_map(|message| filter_message(message, access, &removed_call_ids))
        .collect()
}

fn removed_tool_call_ids(messages: &[Message], access: &ToolAccess) -> HashSet<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { tool_calls, .. } => Some(tool_calls),
            _ => None,
        })
        .flatten()
        .filter(|call| !access.allows(&call.name))
        .map(|call| call.id.clone())
        .collect()
}

fn filter_message(
    message: Message,
    access: &ToolAccess,
    removed_call_ids: &HashSet<String>,
) -> Option<Message> {
    match message {
        Message::Assistant { text, tool_calls } => {
            let tool_calls: Vec<ToolCall> = tool_calls
                .into_iter()
                .filter(|call| access.allows(&call.name))
                .collect();
            if text.is_empty() && tool_calls.is_empty() {
                None
            } else {
                Some(Message::Assistant { text, tool_calls })
            }
        }
        Message::ToolResult { call_id, .. } if removed_call_ids.contains(&call_id) => None,
        other => Some(other),
    }
}

pub(super) async fn drive_one_task<S>(mut ctx: DriveCtx<S>, task_id: TaskId, run_req: RunRequest)
where
    S: TaskStore + 'static,
{
    observe_task_started(&ctx).await;
    let outcome = run_with_timeout(&ctx, &task_id, run_req).await;
    let verdict = classify_outcome(
        &outcome,
        ctx.pause.is_cancelled(),
        ctx.reason.get().copied(),
    );
    match &verdict {
        TaskFinal::Paused(cause) => {
            pause_task(&ctx.store, &task_id, *cause).await;
            observe_task_paused(&ctx, &task_id, *cause).await;
        }
        TaskFinal::Done => {
            finalize_task(&ctx.store, &task_id, TaskStatus::Done, None).await;
            observe_task_terminal(&ctx, &task_id, TaskStatus::Done, None).await;
        }
        TaskFinal::Failed(error) => {
            finalize_task(&ctx.store, &task_id, TaskStatus::Failed, Some(error)).await;
            observe_task_terminal(&ctx, &task_id, TaskStatus::Failed, Some(error)).await;
        }
    }
    signal_completion(&mut ctx, &task_id).await;
    ctx.cancel_handles.lock().await.remove(&task_id);
    if let TaskFinal::Paused(_) = &verdict {
        // Pause is not terminal: completion notifiers stay silent.
        return;
    }
    let error = match &verdict {
        TaskFinal::Failed(error) => Some(error.as_str()),
        TaskFinal::Done | TaskFinal::Paused(_) => None,
    };
    notify_completion(&ctx.store, &task_id, &ctx.notifiers, &outcome, error).await;
}

async fn run_with_timeout<S>(
    ctx: &DriveCtx<S>,
    task_id: &TaskId,
    run_req: RunRequest,
) -> DrainOutcome
where
    S: TaskStore + 'static,
{
    let cancel = ctx.cancel.clone();
    let tool_access = ctx.tool_access.clone();
    let stream = Box::pin(ctx.kernel.run_streaming_with_tool_filter(
        run_req,
        Some(&cancel),
        move |name| tool_access.allows(name),
    ));
    let drain = drain_stream_observed(
        Arc::clone(&ctx.store),
        task_id.clone(),
        stream,
        ctx.output_debounce_bytes,
        ctx.progress_debounce,
        ctx.activity_meta.clone(),
        ctx.observers.clone(),
    );
    drain_timeout::run_drain_with_timeout(task_id, cancel, ctx.timeout, ctx.shutdown_grace, drain)
        .await
}

/// Final routing decision for a drained task run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TaskFinal {
    Done,
    Failed(String),
    Paused(TaskPauseCause),
}

pub(super) fn classify_outcome(
    outcome: &DrainOutcome,
    pause_requested: bool,
    reason: Option<CancelReason>,
) -> TaskFinal {
    if outcome.paused {
        // Cooperative safe-boundary exit (Outcome::Paused). A user cancel
        // that was stamped before classification still wins: cancelled
        // work must never resurrect after a restart.
        if reason == Some(CancelReason::StopPattern) {
            return TaskFinal::Failed(CANCELLED_MESSAGE.to_owned());
        }
        return TaskFinal::Paused(TaskPauseCause::Shutdown);
    }
    match &outcome.error {
        // A task that exhausted its time budget is not resumable: the
        // timeout verdict beats a concurrent pause window.
        Some(error) if error == TIMEOUT_MESSAGE => TaskFinal::Failed(error.clone()),
        // Force-pause: the cancel landed during the executor's pause
        // window and no user cancel intent (StopPattern) was stamped.
        Some(error)
            if error == CANCELLED_MESSAGE
                && pause_requested
                && reason != Some(CancelReason::StopPattern) =>
        {
            TaskFinal::Paused(TaskPauseCause::Forced)
        }
        Some(error) => TaskFinal::Failed(error.clone()),
        // A stream that closed cleanly without an `Event::Final` (e.g. the
        // kernel hit `MaxTurnsExceeded` and dropped the channel) produced no
        // output and no error. Classify that as Failed instead of silently
        // reporting Done: a task with no final text has not completed its work.
        None if outcome.final_text.is_none() => {
            TaskFinal::Failed(NO_FINAL_EVENT_MESSAGE.to_owned())
        }
        None => TaskFinal::Done,
    }
}
