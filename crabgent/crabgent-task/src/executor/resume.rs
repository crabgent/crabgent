//! Restart-time resume of paused tasks: orphan adoption, children-first
//! claim, dangling-tool-call repair, and respawn from the persisted
//! transcript plus [`TaskResumeSpec`].
//!
//! Every step is a CAS or idempotent, so a crash anywhere in the scan
//! converges through orphan adoption on the next startup.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use crabgent_core::ModelId;
use crabgent_core::message::Message;
use crabgent_core::{Kernel, Subject};
use crabgent_log::{info, warn};
use crabgent_store::records::{Task, TaskResumeSpec, TaskStatus};
use crabgent_store::traits::TaskStore;
use crabgent_store::{Page, TaskId};

use crate::error::TaskError;
use crate::request::TaskRequest;

use super::spawn::{acquire_spawn_permit, launch_run};
use super::{RESUME_LIMIT_MESSAGE, TaskExecutor};

const SCAN_PAGE_SIZE: usize = 100;
/// Cap on parent-link hops when computing tree depth, mirroring the
/// depth walk in `crabgent-tool-task`.
const MAX_DEPTH_WALK: usize = 16;
const NOT_RESUMABLE_MESSAGE: &str = "not resumable: missing or invalid resume spec";
/// Length cap for child names embedded in the repair note.
const MAX_CHILD_NAME_CHARS: usize = 80;

pub(super) async fn do_resume_paused<S, F>(
    exec: &TaskExecutor<S>,
    resolve: F,
) -> Result<Vec<TaskId>, TaskError>
where
    S: TaskStore + 'static,
    F: Fn(&Task) -> Option<Arc<Kernel>> + Send,
{
    // Orphan adoption is agent-neutral status recovery: folding a stale
    // Running row into Paused(Crash) does not bind it to this executor.
    // Tasks the resolver declines stay Paused for a sibling executor's
    // scan, and a second adoption pass is an idempotent no-op.
    let adopted = exec.store.pause_orphans(exec.orphan_stale_secs).await?;
    if !adopted.is_empty() {
        info!(
            count = adopted.len(),
            "task resume: adopted stale running tasks as crash-paused"
        );
    }
    let paused = scan_paused(exec).await?;
    // Resolve BEFORE any claim: in multi-agent hosts (one task store,
    // several kernels/executors) a task must be matched to its kernel
    // first, otherwise the first executor would steal every paused task.
    let selected: Vec<(Task, Arc<Kernel>)> = paused
        .into_iter()
        .filter_map(|task| resolve(&task).map(|kernel| (task, kernel)))
        .collect();
    let ordered = order_children_first(selected);

    let mut resumed = Vec::new();
    for (task, kernel) in ordered {
        let task_id = task.id.clone();
        // Per-task failures are skipped, not propagated: one bad row must
        // not strand every later paused task until the next boot.
        match resume_one(exec, &kernel, task).await {
            Ok(Some(task_id)) => resumed.push(task_id),
            Ok(None) => {}
            Err(error) => log_resume_one_failed(&task_id, &error),
        }
    }
    Ok(resumed)
}

fn log_resume_one_failed(task_id: &TaskId, error: &TaskError) {
    warn!(
        task_id = %task_id,
        error = %error,
        "task resume: resuming this task failed; skipping (retried next boot)"
    );
}

async fn scan_paused<S>(exec: &TaskExecutor<S>) -> Result<Vec<Task>, TaskError>
where
    S: TaskStore + 'static,
{
    let mut paused = Vec::new();
    let mut page = Page::first(SCAN_PAGE_SIZE);
    loop {
        let batch = exec.store.list_paused(page).await?;
        let last = batch.len() < page.limit;
        paused.extend(batch);
        if last {
            return Ok(paused);
        }
        page = page.next();
    }
}

/// Sort deepest-first (children before parents) so a resumed blocking
/// parent finds its child already claimed and running when it
/// re-attaches. Depth ties keep oldest-first creation order. Applied to
/// the resolver-selected set; depth is computed over the selected tasks'
/// parent links.
fn order_children_first(mut tasks: Vec<(Task, Arc<Kernel>)>) -> Vec<(Task, Arc<Kernel>)> {
    let parents: HashMap<TaskId, Option<TaskId>> = tasks
        .iter()
        .map(|(task, _)| (task.id.clone(), task.parent_task_id.clone()))
        .collect();
    tasks.sort_by_key(|(task, _)| std::cmp::Reverse(tree_depth(&parents, &task.id)));
    tasks
}

fn tree_depth(parents: &HashMap<TaskId, Option<TaskId>>, id: &TaskId) -> usize {
    let mut depth = 0;
    let mut current = id.clone();
    while depth < MAX_DEPTH_WALK {
        match parents.get(&current) {
            Some(Some(parent)) => {
                depth += 1;
                current = parent.clone();
            }
            _ => break,
        }
    }
    depth
}

/// Claim and respawn one paused task. Returns `Some(id)` when this call
/// won the claim and the task is running again.
async fn resume_one<S>(
    exec: &TaskExecutor<S>,
    kernel: &Arc<Kernel>,
    task: Task,
) -> Result<Option<TaskId>, TaskError>
where
    S: TaskStore + 'static,
{
    let Some(request) = rebuild_request(&task) else {
        // Legacy or corrupt rows (pre-pause deploys, undecodable specs)
        // are deterministically finished instead of looping every boot.
        if exec
            .store
            .claim_for_resume(&task.id, exec.max_resumes)
            .await?
        {
            exec.store
                .finish(&task.id, TaskStatus::Failed, Some(NOT_RESUMABLE_MESSAGE))
                .await?;
        }
        return Ok(None);
    };
    // Acquire the concurrency permit BEFORE the claim so a boot with more
    // paused tasks than max_parallel never holds claimed rows in `Running`
    // while parked on the semaphore.
    let permit = acquire_spawn_permit(exec).await?;
    if !exec
        .store
        .claim_for_resume(&task.id, exec.max_resumes)
        .await?
    {
        fail_when_cap_exhausted(exec, &task.id).await?;
        return Ok(None);
    }

    let transcript = exec
        .store
        .load_transcript(&task.id)
        .await?
        .unwrap_or_default();
    let children = exec
        .store
        .list_children(&task.id, Page::first(SCAN_PAGE_SIZE))
        .await?;
    let (repaired, changed) = repair_dangling(transcript, &children);
    if changed {
        // Persist the repair before spawning so a crash between claim and
        // launch converges: repairing a repaired transcript is a no-op.
        if let Err(error) = exec.store.save_transcript(&task.id, &repaired).await {
            log_repair_save_failed(&task.id, &error);
        }
    }
    let request = TaskRequest {
        messages: repaired,
        ..request
    };
    let task_id = launch_run(exec, Arc::clone(kernel), &request, task, permit, None).await;
    Ok(Some(task_id))
}

fn log_repair_save_failed(task_id: &TaskId, error: &crabgent_store::StoreError) {
    warn!(
        task_id = %task_id,
        error = %error,
        "task resume: persisting repaired transcript failed; resuming from memory"
    );
}

/// A `false` claim can mean a lost race, a task that left `Paused`, or an
/// exhausted resume cap. Only the cap case needs action: finish the task
/// so it stops cycling through every boot.
async fn fail_when_cap_exhausted<S>(
    exec: &TaskExecutor<S>,
    task_id: &TaskId,
) -> Result<(), TaskError>
where
    S: TaskStore + 'static,
{
    let Some(current) = exec.store.get(task_id).await? else {
        return Ok(());
    };
    if current.status == TaskStatus::Paused
        && current.pause_cause != Some(crabgent_store::TaskPauseCause::Shutdown)
        && current.resume_count >= exec.max_resumes
    {
        warn!(
            task_id = %task_id,
            resume_count = current.resume_count,
            "task resume: resume limit exceeded; failing poison task"
        );
        exec.store
            .finish(task_id, TaskStatus::Failed, Some(RESUME_LIMIT_MESSAGE))
            .await?;
    }
    Ok(())
}

/// Rebuild the spawn-equivalent [`TaskRequest`] from the persisted record
/// and its resume spec. `None` when the row is not resumable.
fn rebuild_request(task: &Task) -> Option<TaskRequest> {
    let spec = task.resume_spec.as_ref()?;
    let subject = rebuild_subject(spec)?;
    Some(TaskRequest {
        owner: task.owner.clone(),
        subject,
        name: task.name.clone(),
        prompt: task.prompt.clone(),
        model: spec.model.clone().into(),
        explicit_model: spec.explicit_model.clone().map(Into::into),
        session_model_override: spec.session_model_override.clone().map(ModelId::new),
        reasoning_effort: spec.reasoning_effort,
        system_prompt: spec.system_prompt.clone(),
        messages: Vec::new(),
        parent_session_id: task.parent_session_id.clone(),
        parent_task_id: task.parent_task_id.clone(),
        context_mode: task.context_mode.clone(),
        max_turns: spec.max_turns,
        tool_access: spec.tool_access.clone(),
    })
}

fn rebuild_subject(spec: &TaskResumeSpec) -> Option<Subject> {
    let mut subject = Subject::try_new(&spec.subject_id).ok()?;
    for (key, value) in &spec.subject_attrs {
        subject = subject.with_attr(key.clone(), value.clone());
    }
    Some(subject)
}

/// Append a synthetic error [`Message::ToolResult`] for every tool call
/// in the transcript that has no matching result (fixed user decision:
/// interrupted calls are surfaced to the LLM, never silently re-run).
/// Task-tool calls additionally embed this task's children so a resumed
/// parent re-attaches via `task get` instead of re-spawning work.
/// Idempotent: repaired transcripts have no dangling ids left.
pub(super) fn repair_dangling(
    mut transcript: Vec<Message>,
    children: &[Task],
) -> (Vec<Message>, bool) {
    let dangling = dangling_calls(&transcript);
    if dangling.is_empty() {
        return (transcript, false);
    }
    for (call_id, tool_name) in dangling {
        let note = interrupted_note(&tool_name, children);
        transcript.push(Message::ToolResult {
            call_id,
            output: serde_json::Value::String(note),
            is_error: true,
        });
    }
    (transcript, true)
}

/// Collect `(call_id, tool_name)` of assistant tool calls without a
/// matching `ToolResult` anywhere in the transcript.
fn dangling_calls(transcript: &[Message]) -> Vec<(String, String)> {
    let resolved: std::collections::HashSet<&str> = transcript
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    transcript
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { tool_calls, .. } => Some(tool_calls),
            _ => None,
        })
        .flatten()
        .filter(|call| !resolved.contains(call.id.as_str()))
        .map(|call| (call.id.clone(), call.name.clone()))
        .collect()
}

fn interrupted_note(tool_name: &str, children: &[Task]) -> String {
    let mut note = String::from(
        "interrupted by shutdown/restart before a result was recorded; \
         the call may have partially executed. Decide what (if anything) to redo.",
    );
    if tool_name == "task" && !children.is_empty() {
        note.push_str(
            "\nTasks already spawned by this conversation (re-attach via task get instead of re-spawning):",
        );
        for child in children {
            // Writing into a String is infallible.
            let _infallible = write!(
                note,
                "\n- {} (name: {}, status: {})",
                child.id,
                sanitize_child_name(child.name.as_deref().unwrap_or("-")),
                child.status.as_str()
            );
        }
        if children.len() >= SCAN_PAGE_SIZE {
            note.push_str("\n(child list truncated)");
        }
    }
    note
}

/// Child names are LLM-supplied tool args; strip control characters
/// (newline forgery into the model-read repair note) and cap the length.
fn sanitize_child_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(MAX_CHILD_NAME_CHARS)
        .collect()
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
