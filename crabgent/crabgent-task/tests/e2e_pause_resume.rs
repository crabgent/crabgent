//! End-to-end pause/resume: a task pauses cooperatively during
//! `TaskExecutor::shutdown`, its transcript survives via
//! `TaskTranscriptHook`, and a fresh executor resumes it to completion
//! with `resume_paused`. Plus the resume-scan edge cases: orphan
//! adoption, the resume cap, and legacy rows without a resume spec.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_core::model::ModelInfo;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::tool::{Tool, ToolCtx};
use crabgent_core::{Kernel, ToolError};
use crabgent_store::memory::MemoryTaskStore;
use crabgent_store::records::{Task, TaskPauseCause, TaskResumeSpec, TaskStatus};
use crabgent_store::traits::TaskStore;
use crabgent_store::{ModelTargetDto, Owner, TaskId};
use crabgent_task::{TaskExecutor, TaskRequest, TaskTranscriptHook};
use crabgent_test_support::{StubProvider, tool_call, tool_use};
use serde_json::{Value, json};

struct SlowTool;

#[async_trait]
impl Tool for SlowTool {
    fn name(&self) -> &'static str {
        "slow"
    }

    fn description(&self) -> &'static str {
        "test tool that sleeps briefly"
    }

    fn parameters_schema(&self) -> Value {
        json!({})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(json!({"ok": true}))
    }
}

/// Kernel whose provider keeps issuing slow tool calls and finally
/// completes, with the transcript hook wired for crash-safe persistence.
fn pausable_kernel(store: &Arc<MemoryTaskStore>, tool_turns: usize) -> Arc<Kernel> {
    pausable_kernel_with_finals(store, tool_turns, 1)
}

/// Like [`pausable_kernel`] but with `finals` terminal responses so the
/// shared stub provider can serve that many independent task runs.
fn pausable_kernel_with_finals(
    store: &Arc<MemoryTaskStore>,
    tool_turns: usize,
    finals: usize,
) -> Arc<Kernel> {
    let mut responses: Vec<_> = (0..tool_turns)
        .map(|i| tool_use(vec![tool_call(format!("c{i}"), "slow", json!({}))]))
        .collect();
    responses.extend((0..finals).map(|_| crabgent_test_support::done("after-resume")));
    Arc::new(
        Kernel::builder()
            .provider(
                StubProvider::new()
                    .responses(responses)
                    .with_tools(true)
                    .with_models(vec![ModelInfo::minimal("m", "stub")]),
            )
            .policy(AllowAllPolicy)
            .add_tool(SlowTool)
            .add_hook(TaskTranscriptHook::new(Arc::clone(store)))
            .build(),
    )
}

fn executor(store: &Arc<MemoryTaskStore>) -> TaskExecutor<MemoryTaskStore> {
    TaskExecutor::new(Arc::clone(store))
        .with_timeout(Duration::from_mins(1))
        .with_pause_grace(Duration::from_secs(5))
        .with_shutdown_grace(Duration::from_millis(500))
}

async fn wait_for_status(store: &Arc<MemoryTaskStore>, id: &TaskId, status: TaskStatus) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let task = store.get(id).await.expect("get task").expect("task exists");
        if task.status == status {
            return task;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task never reached {status:?}: {task:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_cycle_pause_on_shutdown_then_resume_to_done() {
    let store = Arc::new(MemoryTaskStore::default());
    let kernel = pausable_kernel(&store, 40);
    let exec = executor(&store);

    let id = exec
        .spawn(
            Arc::clone(&kernel),
            TaskRequest::new(Owner::new("u"), "m", "p"),
        )
        .await
        .expect("spawn");

    // Wait until at least one turn landed in the persisted transcript.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if store
            .load_transcript(&id)
            .await
            .expect("load transcript")
            .is_some_and(|t| t.len() >= 2)
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no transcript");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    exec.shutdown().await;

    let paused = wait_for_status(&store, &id, TaskStatus::Paused).await;
    assert_eq!(paused.pause_cause, Some(TaskPauseCause::Shutdown));
    assert!(paused.paused_at.is_some());
    assert!(paused.finished_at.is_none());

    // New spawns are rejected after shutdown started.
    let rejected = exec
        .spawn(
            Arc::clone(&kernel),
            TaskRequest::new(Owner::new("u"), "m", "p"),
        )
        .await;
    rejected.expect_err("spawn after shutdown is rejected");

    // Fresh executor (same store, same kernel double): resume runs the
    // task to completion from its persisted transcript.
    let exec2 = executor(&store);
    let resumed = exec2
        .resume_paused(Arc::clone(&kernel))
        .await
        .expect("resume scan");
    assert_eq!(resumed, vec![id.clone()]);

    let done = wait_for_status(&store, &id, TaskStatus::Done).await;
    assert_eq!(
        done.resume_count, 0,
        "clean shutdown pauses never burn the resume cap"
    );
    assert!(done.pause_cause.is_none());
    assert!(done.output.contains("after-resume") || done.error.is_none());

    // Idempotent: a second scan finds nothing.
    let again = exec2
        .resume_paused(Arc::clone(&kernel))
        .await
        .expect("resume scan");
    assert!(again.is_empty());
    exec2.shutdown().await;
}

fn paused_record(store_suffix: &str, resume_count: u32, with_spec: bool) -> Task {
    let now = Utc::now();
    Task {
        id: TaskId::new(),
        owner: Owner::new(format!("u-{store_suffix}")),
        name: None,
        prompt: "continue the work".into(),
        status: TaskStatus::Paused,
        output: String::new(),
        error: None,
        created_at: now,
        updated_at: now,
        finished_at: None,
        parent_session_id: None,
        parent_task_id: None,
        context_mode: None,
        reasoning_effort_override: None,
        resume_spec: with_spec.then(|| TaskResumeSpec {
            subject_id: format!("u-{store_suffix}"),
            subject_attrs: std::collections::HashMap::new(),
            model: ModelTargetDto::Id("m".into()),
            explicit_model: Some(ModelTargetDto::Id("m".into())),
            session_model_override: None,
            reasoning_effort: None,
            system_prompt: None,
            max_turns: Some(10),
            tool_access: crabgent_core::ToolAccess::all(),
        }),
        resume_count,
        pause_cause: Some(TaskPauseCause::Shutdown),
        paused_at: Some(now),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphaned_running_task_is_adopted_and_resumed() {
    let store = Arc::new(MemoryTaskStore::default());
    let kernel = pausable_kernel(&store, 0);
    let mut orphan = paused_record("orphan", 0, true);
    orphan.status = TaskStatus::Running;
    orphan.pause_cause = None;
    orphan.paused_at = None;
    orphan.updated_at = Utc::now() - chrono::Duration::hours(1);
    store.insert(&orphan).await.expect("insert");

    let exec = executor(&store);
    let resumed = exec.resume_paused(Arc::clone(&kernel)).await.expect("scan");

    assert_eq!(resumed, vec![orphan.id.clone()]);
    let done = wait_for_status(&store, &orphan.id, TaskStatus::Done).await;
    assert_eq!(done.resume_count, 1);
    exec.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_cap_fails_poison_task() {
    let store = Arc::new(MemoryTaskStore::default());
    let kernel = pausable_kernel(&store, 0);
    let mut poison = paused_record("poison", 3, true);
    // Only involuntary pauses (crash/forced) count toward the cap.
    poison.pause_cause = Some(TaskPauseCause::Crash);
    store.insert(&poison).await.expect("insert");

    let exec = executor(&store);
    let resumed = exec.resume_paused(Arc::clone(&kernel)).await.expect("scan");

    assert!(resumed.is_empty());
    let failed = wait_for_status(&store, &poison.id, TaskStatus::Failed).await;
    assert_eq!(failed.error.as_deref(), Some("resume limit exceeded"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_row_without_resume_spec_fails_deterministically() {
    let store = Arc::new(MemoryTaskStore::default());
    let kernel = pausable_kernel(&store, 0);
    let legacy = paused_record("legacy", 0, false);
    store.insert(&legacy).await.expect("insert");

    let exec = executor(&store);
    let resumed = exec.resume_paused(Arc::clone(&kernel)).await.expect("scan");

    assert!(resumed.is_empty());
    let failed = wait_for_status(&store, &legacy.id, TaskStatus::Failed).await;
    assert_eq!(
        failed.error.as_deref(),
        Some("not resumable: missing or invalid resume spec")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn children_resume_before_parents() {
    let store = Arc::new(MemoryTaskStore::default());
    let kernel = pausable_kernel_with_finals(&store, 0, 2);
    let parent = paused_record("parent", 0, true);
    let mut child = paused_record("child", 0, true);
    child.parent_task_id = Some(parent.id.clone());
    // Insert parent first so creation order alone would resume it first.
    store.insert(&parent).await.expect("insert");
    store.insert(&child).await.expect("insert");

    let exec = executor(&store);
    let resumed = exec.resume_paused(Arc::clone(&kernel)).await.expect("scan");

    assert_eq!(resumed, vec![child.id.clone(), parent.id.clone()]);
    wait_for_status(&store, &child.id, TaskStatus::Done).await;
    wait_for_status(&store, &parent.id, TaskStatus::Done).await;
    exec.shutdown().await;
}

fn agent_of(task: &Task) -> Option<&str> {
    task.resume_spec
        .as_ref()
        .and_then(|spec| spec.subject_attrs.get("agent"))
        .map(String::as_str)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_agent_resolver_resumes_only_matching_tasks() {
    let store = Arc::new(MemoryTaskStore::default());
    // Two kernels with distinguishable outputs standing in for two agents.
    let kernel_a = pausable_kernel(&store, 0);
    let kernel_b = pausable_kernel(&store, 0);

    let mut task_a = paused_record("agent-a", 0, true);
    if let Some(spec) = task_a.resume_spec.as_mut() {
        spec.subject_attrs
            .insert("agent".to_owned(), "alpha".to_owned());
    }
    let mut task_b = paused_record("agent-b", 0, true);
    if let Some(spec) = task_b.resume_spec.as_mut() {
        spec.subject_attrs
            .insert("agent".to_owned(), "beta".to_owned());
    }
    store.insert(&task_a).await.expect("insert");
    store.insert(&task_b).await.expect("insert");

    // Executor for agent alpha resumes only alpha's task...
    let exec_a = executor(&store);
    let kernel = Arc::clone(&kernel_a);
    let resumed_a = exec_a
        .resume_paused_with(move |task| {
            (agent_of(task) == Some("alpha")).then(|| Arc::clone(&kernel))
        })
        .await
        .expect("alpha scan");
    assert_eq!(resumed_a, vec![task_a.id.clone()]);

    // ...and beta's task is still untouched (Paused) for its own executor.
    let b_after_a = store.get(&task_b.id).await.expect("get").expect("exists");
    assert_eq!(
        b_after_a.status,
        TaskStatus::Paused,
        "no executor steals another agent's task"
    );

    let exec_b = executor(&store);
    let kernel = Arc::clone(&kernel_b);
    let resumed_b = exec_b
        .resume_paused_with(move |task| {
            (agent_of(task) == Some("beta")).then(|| Arc::clone(&kernel))
        })
        .await
        .expect("beta scan");
    assert_eq!(resumed_b, vec![task_b.id.clone()]);

    wait_for_status(&store, &task_a.id, TaskStatus::Done).await;
    wait_for_status(&store, &task_b.id, TaskStatus::Done).await;
    exec_a.shutdown().await;
    exec_b.shutdown().await;
}
