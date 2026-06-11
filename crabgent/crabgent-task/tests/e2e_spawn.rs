//! End-to-end test for [`TaskExecutor::spawn`].
//!
//! Drives a real `Kernel` with a stub provider, confirms the spawned
//! task transitions to `Done`, the final assistant text lands in the
//! task output, and the configured notifier fires exactly once.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_core::error::ProviderError;
use crabgent_core::model::{ModelInfo, ResolvedSource};
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::tool::{Tool, ToolCtx};
use crabgent_core::types::{LlmRequest, LlmResponse, StopReason, ToolCall, Usage};
use crabgent_core::{Kernel, RunCtx, Subject, ToolError};
use crabgent_store::Owner;
use crabgent_store::memory::MemoryTaskStore;
use crabgent_store::records::{Task, TaskStatus};
use crabgent_store::traits::TaskStore;
use crabgent_task::{TaskError, TaskExecutor, TaskNotifier, TaskRequest};
use crabgent_test_support::StubProvider;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// A `StubProvider` returning `text`, advertising the three models the spawn
/// tests route across.
fn stub_provider(text: &str) -> StubProvider {
    StubProvider::with_text(text).with_models(vec![
        ModelInfo::minimal("claude-haiku-4-5", "stub"),
        ModelInfo::minimal("m", "stub"),
        ModelInfo::minimal("session-model", "stub"),
    ])
}

struct SourceProbeProvider {
    calls: Mutex<u8>,
}

#[async_trait]
impl Provider for SourceProbeProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let mut calls = self.calls.lock().expect("calls mutex");
        *calls += 1;
        if *calls == 1 {
            return Ok(LlmResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "source-1".to_owned(),
                    name: "record_source".to_owned(),
                    args: json!({}),
                    thought_signature: None,
                }],
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                model: req.model.clone(),
            });
        }
        Ok(LlmResponse {
            text: req.model.to_string(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "stub"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo::minimal("default-model", "stub"),
            ModelInfo::minimal("session-model", "stub"),
        ]
    }
}

struct SourceProbeTool {
    seen: Arc<Mutex<Vec<ResolvedSource>>>,
}

#[async_trait]
impl Tool for SourceProbeTool {
    fn name(&self) -> &'static str {
        "record_source"
    }

    fn description(&self) -> &'static str {
        "record current model source"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let current = ctx.current_model.as_ref().expect("current model context");
        self.seen.lock().expect("seen mutex").push(current.source);
        Ok(json!({ "source": current.source.as_str() }))
    }
}

struct CancelAwareProvider {
    started: Mutex<Option<oneshot::Sender<()>>>,
    cancelled: Mutex<Option<oneshot::Sender<()>>>,
}

#[async_trait]
impl Provider for CancelAwareProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let started = self.started.lock().expect("started mutex").take();
        if let Some(tx) = started
            && tx.send(()).is_err()
        {
            // Receiver was dropped by the test after observing enough state.
        }
        let Some(cancel) = cancel else {
            return Err(ProviderError::Other("missing cancel token".into()));
        };
        cancel.cancelled().await;
        let cancelled = self.cancelled.lock().expect("cancelled mutex").take();
        if let Some(tx) = cancelled
            && tx.send(()).is_err()
        {
            // Receiver was dropped by the test after observing enough state.
        }
        Err(ProviderError::Cancelled)
    }
    fn name(&self) -> &'static str {
        "cancel-aware"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("m", "cancel-aware")]
    }
}

struct OnceNotifier {
    tx: Mutex<Option<oneshot::Sender<TaskNotification>>>,
}

struct TaskNotification {
    task: Task,
    message: String,
}

#[async_trait]
impl TaskNotifier for OnceNotifier {
    async fn notify(&self, task: &Task, message: &str) -> Result<bool, TaskError> {
        let mut guard = self.tx.lock().expect("notifier mutex");
        if let Some(tx) = guard.take()
            && tx
                .send(TaskNotification {
                    task: task.clone(),
                    message: message.into(),
                })
                .is_err()
        {
            // Receiver was dropped by the test after observing enough state.
        }
        Ok(true)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_runs_kernel_to_done_and_dispatches_notifier() {
    let store = Arc::new(MemoryTaskStore::default());
    let kernel = Arc::new(
        Kernel::builder()
            .provider(stub_provider("hello world"))
            .policy(AllowAllPolicy)
            .build(),
    );
    let (tx, rx) = oneshot::channel();
    let notifier: Arc<dyn TaskNotifier> = Arc::new(OnceNotifier {
        tx: Mutex::new(Some(tx)),
    });
    let exec = TaskExecutor::new(Arc::clone(&store))
        .with_timeout(Duration::from_secs(5))
        .with_progress_debounce(Duration::from_millis(10))
        .with_output_debounce_bytes(4)
        .with_notifier(notifier);

    let req = TaskRequest::new(Owner::new("alice"), "claude-haiku-4-5", "say hi")
        .with_subject(Subject::new("alice"))
        .with_max_turns(2);
    let id = exec
        .spawn(Arc::clone(&kernel), req)
        .await
        .expect("spawn returns id");

    let notification = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("notifier must fire within 5s")
        .expect("notifier sender dropped");
    assert_eq!(notification.task.id, id);
    assert_eq!(notification.task.status, TaskStatus::Done);
    assert_eq!(notification.message, "hello world");

    let stored = store
        .get(&id)
        .await
        .expect("store get")
        .expect("task exists");
    assert_eq!(stored.status, TaskStatus::Done);
    assert!(stored.finished_at.is_some());
    assert!(stored.output.contains("hello world"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_without_explicit_model_uses_session_override_source() {
    let store = Arc::new(MemoryTaskStore::default());
    let seen_sources = Arc::new(Mutex::new(Vec::new()));
    let kernel = Arc::new(
        Kernel::builder()
            .provider(SourceProbeProvider {
                calls: Mutex::new(0),
            })
            .add_tool(SourceProbeTool {
                seen: Arc::clone(&seen_sources),
            })
            .policy(AllowAllPolicy)
            .build(),
    );
    let exec = TaskExecutor::new(Arc::clone(&store))
        .with_timeout(Duration::from_secs(5))
        .with_progress_debounce(Duration::from_millis(10));
    let req = TaskRequest::new_default(Owner::new("alice"), "default-model", "say hi")
        .with_session_model_override("session-model")
        .with_max_turns(3);

    let id = exec
        .spawn(Arc::clone(&kernel), req)
        .await
        .expect("spawn returns id");
    let stored = wait_for_task_status(&store, &id, TaskStatus::Done).await;

    assert_eq!(stored.output, "session-model");
    assert_eq!(
        *seen_sources.lock().expect("seen sources mutex"),
        vec![ResolvedSource::SessionOverride]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn error_text_persisted_after_failure() {
    let store = Arc::new(MemoryTaskStore::default());
    let kernel = Arc::new(
        Kernel::builder()
            .provider(StubProvider::new().fail_with(|| ProviderError::Auth("no token".into())))
            .policy(AllowAllPolicy)
            .build(),
    );
    let (sender, receiver) = oneshot::channel();
    let notifier: Arc<dyn TaskNotifier> = Arc::new(OnceNotifier {
        tx: Mutex::new(Some(sender)),
    });
    let exec = TaskExecutor::new(Arc::clone(&store))
        .with_timeout(Duration::from_secs(5))
        .with_progress_debounce(Duration::from_millis(10))
        .with_notifier(notifier);

    let id = exec
        .spawn(
            Arc::clone(&kernel),
            TaskRequest::new(Owner::new("u"), "m", "p"),
        )
        .await
        .expect("spawn returns id");

    let notification = tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .expect("notifier must fire within 5s")
        .expect("notifier sender dropped");
    assert_eq!(notification.task.id, id);
    assert_eq!(notification.task.status, TaskStatus::Failed);
    assert_eq!(
        notification.task.error.as_deref(),
        Some("provider error: auth error: no token")
    );
    assert!(
        notification
            .message
            .contains("provider error: auth error: no token")
    );
    let stored = store
        .get(&id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(stored.status, TaskStatus::Failed);
    assert_eq!(
        stored.error.as_deref(),
        Some("provider error: auth error: no token")
    );
}

async fn wait_for_task_status(
    store: &Arc<MemoryTaskStore>,
    id: &crabgent_store::TaskId,
    status: TaskStatus,
) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let task = store
            .get(id)
            .await
            .expect("test result")
            .expect("test result");
        if task.status == status {
            return task;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task did not reach expected status"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn cancel_aware_kernel() -> (Arc<Kernel>, oneshot::Receiver<()>, oneshot::Receiver<()>) {
    let (started_tx, started_rx) = oneshot::channel();
    let (cancelled_tx, cancelled_rx) = oneshot::channel();
    let kernel = Arc::new(
        Kernel::builder()
            .provider(CancelAwareProvider {
                started: Mutex::new(Some(started_tx)),
                cancelled: Mutex::new(Some(cancelled_tx)),
            })
            .policy(AllowAllPolicy)
            .build(),
    );
    (kernel, started_rx, cancelled_rx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hung_task_force_paused_after_shutdown_drains_joinset() {
    let store = Arc::new(MemoryTaskStore::default());
    let (kernel, started_rx, _cancelled_rx) = cancel_aware_kernel();
    let exec = TaskExecutor::new(Arc::clone(&store))
        .with_timeout(Duration::from_mins(1))
        .with_pause_grace(Duration::from_millis(50))
        .with_shutdown_grace(Duration::from_millis(100));
    let id = exec
        .spawn(
            Arc::clone(&kernel),
            TaskRequest::new(Owner::new("u"), "m", "p"),
        )
        .await
        .expect("spawn returns id");

    tokio::time::timeout(Duration::from_secs(1), started_rx)
        .await
        .expect("provider should start")
        .expect("started sender should live");
    exec.shutdown().await;

    assert_eq!(exec.in_flight().await, 0);
    let stored = store
        .get(&id)
        .await
        .expect("test result")
        .expect("test result");
    // A task that never reaches a safe pause boundary is force-paused
    // (resumable after restart), not failed: shutdown is not a verdict
    // on the task's work.
    assert_eq!(stored.status, TaskStatus::Paused);
    assert_eq!(
        stored.pause_cause,
        Some(crabgent_store::TaskPauseCause::Forced)
    );
    assert!(stored.error.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_text_persisted_after_drain_grace() {
    let store = Arc::new(MemoryTaskStore::default());
    let (kernel, started_rx, _cancelled_rx) = cancel_aware_kernel();
    let exec = TaskExecutor::new(Arc::clone(&store))
        .with_timeout(Duration::from_millis(20))
        .with_shutdown_grace(Duration::from_millis(100));
    let id = exec
        .spawn(
            Arc::clone(&kernel),
            TaskRequest::new(Owner::new("u"), "m", "p"),
        )
        .await
        .expect("spawn returns id");

    tokio::time::timeout(Duration::from_secs(1), started_rx)
        .await
        .expect("provider should start")
        .expect("started sender should live");
    let stored = wait_for_task_status(&store, &id, TaskStatus::Failed).await;
    assert_eq!(stored.status, TaskStatus::Failed);
    assert_eq!(stored.error.as_deref(), Some("task timed out"));
}
