use super::blocking::do_spawn_blocking;
use super::cancel::do_cancel;
use super::finalize::finalize_task;
use super::spawn::{
    DriveCtx, build_run_request, build_task_record, do_spawn, filter_messages_for_tool_access,
};
use super::*;

use std::sync::OnceLock;
use tokio_util::sync::CancellationToken;

use async_trait::async_trait;
use crabgent_core::message::Message;
use crabgent_core::model::ModelInfo;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse, StopReason, ToolCall, Usage};
use crabgent_core::{
    ContentBlock, Kernel, KernelBuilder, ProviderError, ReasoningEffort, RunCtx, Subject, Tool,
    ToolAccess, ToolCtx, ToolError,
};
use crabgent_store::Owner;
use crabgent_store::memory::MemoryTaskStore;
use crabgent_store::records::{Task, TaskStatus};
use crabgent_store::traits::TaskStore;
use crabgent_test_support::StubProvider;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

fn req(owner: &str) -> TaskRequest {
    TaskRequest::new(Owner::new(owner), "claude-haiku-4-5", "do it")
}

fn build_test_kernel() -> Arc<Kernel> {
    Arc::new(
        KernelBuilder::new()
            .provider(
                StubProvider::with_text("done")
                    .with_models(vec![ModelInfo::minimal("claude-haiku-4-5", "stub")]),
            )
            .policy(AllowAllPolicy)
            .build(),
    )
}

fn cancel_kernel(started_tx: oneshot::Sender<()>) -> Arc<Kernel> {
    Arc::new(
        KernelBuilder::new()
            .provider(CancelAwareProvider {
                started: std::sync::Mutex::new(Some(started_tx)),
            })
            .policy(AllowAllPolicy)
            .build(),
    )
}

async fn get_task(store: &MemoryTaskStore, id: &TaskId) -> Task {
    store.get(id).await.expect("get task").expect("task exists")
}

fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        args: json!({}),
        thought_signature: None,
    }
}

fn user_message(text: &str) -> Message {
    Message::User {
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        timestamp: None,
    }
}

fn tool_result(call_id: &str) -> Message {
    Message::ToolResult {
        call_id: call_id.to_owned(),
        output: json!({"ok": true}),
        is_error: false,
    }
}

fn assistant_message(text: &str, calls: Vec<ToolCall>) -> Message {
    Message::Assistant {
        text: text.to_owned(),
        tool_calls: calls,
    }
}

#[test]
fn build_task_record_carries_request_fields() {
    let r = req("alice");
    let t = build_task_record(&r);
    assert_eq!(t.owner, Owner::new("alice"));
    assert_eq!(t.prompt, "do it");
    assert!(matches!(t.status, TaskStatus::Running));
    assert!(t.error.is_none());
    assert!(t.output.is_empty());
    assert!(t.parent_session_id.is_none());
    assert_eq!(t.reasoning_effort_override, None);
    assert_eq!(
        t.resume_spec.as_ref().expect("resume spec").tool_access,
        ToolAccess::all()
    );
}

#[test]
fn filter_messages_tool_access_all_keeps_messages_exactly() {
    let messages = vec![
        user_message("start"),
        assistant_message("calling", vec![tool_call("call-1", "bash")]),
        tool_result("call-1"),
    ];

    let filtered = filter_messages_for_tool_access(messages.clone(), &ToolAccess::all());

    assert_eq!(filtered, messages);
}

#[test]
fn filter_messages_tool_access_none_removes_tool_history() {
    let messages = vec![
        Message::System {
            content: "system".to_owned(),
        },
        user_message("start"),
        Message::ChannelOutbound {
            conv: Owner::new("alice"),
            body: "sent".to_owned(),
            channel: "test".to_owned(),
            message_id: "m1".to_owned(),
            thread_root: None,
            broadcast: false,
        },
        assistant_message("plain text", Vec::new()),
        assistant_message("calling", vec![tool_call("call-1", "bash")]),
        tool_result("call-1"),
        assistant_message("", vec![tool_call("call-2", "task")]),
        tool_result("call-2"),
        Message::ProviderBlock {
            provider: "stub".to_owned(),
            block: json!({"kind": "server-tool-result"}),
        },
    ];

    let filtered = filter_messages_for_tool_access(messages, &ToolAccess::none());

    assert_eq!(
        filtered,
        vec![
            Message::System {
                content: "system".to_owned(),
            },
            user_message("start"),
            Message::ChannelOutbound {
                conv: Owner::new("alice"),
                body: "sent".to_owned(),
                channel: "test".to_owned(),
                message_id: "m1".to_owned(),
                thread_root: None,
                broadcast: false,
            },
            assistant_message("plain text", Vec::new()),
            assistant_message("calling", Vec::new()),
            Message::ProviderBlock {
                provider: "stub".to_owned(),
                block: json!({"kind": "server-tool-result"}),
            },
        ]
    );
}

#[test]
fn filter_messages_tool_access_only_keeps_allowed_call_results() {
    let messages = vec![
        user_message("start"),
        assistant_message(
            "",
            vec![tool_call("call-1", "bash"), tool_call("call-2", "task")],
        ),
        tool_result("call-1"),
        tool_result("call-2"),
        tool_result("orphan"),
    ];

    let filtered = filter_messages_for_tool_access(messages, &ToolAccess::only(["task"]));

    assert_eq!(
        filtered,
        vec![
            user_message("start"),
            assistant_message("", vec![tool_call("call-2", "task")]),
            tool_result("call-2"),
            tool_result("orphan"),
        ]
    );
}

#[test]
fn build_run_request_inserts_user_message_when_messages_empty() {
    let r = req("u");
    let task = build_task_record(&r);
    let rr = build_run_request(
        &r,
        &task,
        CancellationToken::new(),
        Arc::new(OnceLock::new()),
    );
    assert_eq!(rr.messages.len(), 1);
    assert!(matches!(rr.messages[0], Message::User { .. }));
    assert_eq!(rr.model.as_str(), "claude-haiku-4-5");
    assert_eq!(rr.explicit_model.as_ref(), Some(&r.model));
    assert!(rr.session_model_override.is_none());
    assert_eq!(rr.reasoning_effort, None);
}

#[test]
fn build_task_record_and_run_request_carry_reasoning_effort() {
    let r = req("u").with_reasoning_effort(ReasoningEffort::High);
    let task = build_task_record(&r);
    let rr = build_run_request(
        &r,
        &task,
        CancellationToken::new(),
        Arc::new(OnceLock::new()),
    );

    assert_eq!(task.reasoning_effort_override, Some(ReasoningEffort::High));
    assert_eq!(rr.reasoning_effort, Some(ReasoningEffort::High));
}

#[test]
fn build_task_record_carries_tool_access_in_resume_spec() {
    let r = req("u").with_tool_access(ToolAccess::only(["task"]));
    let task = build_task_record(&r);

    assert_eq!(
        task.resume_spec.expect("resume spec").tool_access,
        ToolAccess::only(["task"])
    );
}

#[test]
fn build_run_request_uses_explicit_messages_when_present() {
    let pre = vec![Message::User {
        content: vec![ContentBlock::Text { text: "pre".into() }],
        timestamp: None,
    }];
    let r = req("u").with_messages(pre);
    let task = build_task_record(&r);
    let rr = build_run_request(
        &r,
        &task,
        CancellationToken::new(),
        Arc::new(OnceLock::new()),
    );
    assert_eq!(rr.messages.len(), 1);
}

#[test]
fn build_run_request_forwards_system_prompt_and_max_turns() {
    let r = req("u").with_system_prompt("be terse").with_max_turns(7);
    let task = build_task_record(&r);
    let rr = build_run_request(
        &r,
        &task,
        CancellationToken::new(),
        Arc::new(OnceLock::new()),
    );
    assert_eq!(rr.system_prompt.as_deref(), Some("be terse"));
    assert_eq!(rr.max_turns, Some(7));
}

#[test]
fn parent_task_id_injected_into_subject_attrs() {
    let r = req("u");
    let task = build_task_record(&r);
    let expected = task.id.to_string();
    let rr = build_run_request(
        &r,
        &task,
        CancellationToken::new(),
        Arc::new(OnceLock::new()),
    );
    assert_eq!(rr.subject.attr("parent_task_id"), Some(expected.as_str()));
}

#[test]
fn executor_default_uses_policy_limits_and_timeouts() {
    let store = Arc::new(MemoryTaskStore::default());
    let exec = TaskExecutor::new(store);
    assert_eq!(exec.timeout, Duration::from_mins(5));
    assert_eq!(exec.shutdown_grace, Duration::from_secs(5));
    assert_eq!(exec.progress_debounce, Duration::from_millis(500));
    assert_eq!(exec.output_debounce_bytes, 256);
    assert_eq!(exec.max_depth(), DEFAULT_MAX_DEPTH);
    assert_eq!(exec.max_parallel(), DEFAULT_MAX_PARALLEL);
    assert_eq!(exec.shutdown_grace(), Duration::from_secs(5));
    assert_eq!(exec.semaphore.available_permits(), DEFAULT_MAX_PARALLEL);
    let drive_ctx_size = size_of::<Option<DriveCtx<MemoryTaskStore>>>();
    assert!(drive_ctx_size > 0);
    assert!(exec.notifiers.is_empty());
    assert!(exec.observers.is_empty());
}

#[test]
fn executor_builder_chains_correctly() {
    use crate::notifier::NoopNotifier;
    use crate::observer::NoopTaskObserver;
    let parent = CancellationToken::new();
    let store = Arc::new(MemoryTaskStore::default());
    let exec = TaskExecutor::new(store)
        .with_timeout(Duration::from_secs(10))
        .with_shutdown_grace(Duration::from_millis(25))
        .with_cancel(&parent)
        .with_progress_debounce(Duration::from_millis(50))
        .with_output_debounce_bytes(8)
        .with_max_depth(2)
        .with_max_parallel(1)
        .with_notifier(Arc::new(NoopNotifier))
        .with_observer(Arc::new(NoopTaskObserver));
    assert_eq!(exec.timeout, Duration::from_secs(10));
    assert_eq!(exec.shutdown_grace, Duration::from_millis(25));
    assert_eq!(exec.progress_debounce, Duration::from_millis(50));
    assert_eq!(exec.output_debounce_bytes, 8);
    assert_eq!(exec.max_depth(), 2);
    assert_eq!(exec.max_parallel(), 1);
    assert_eq!(exec.semaphore.available_permits(), 1);
    assert_eq!(exec.notifiers.len(), 1);
    assert_eq!(exec.observers.len(), 1);
}

#[tokio::test]
async fn spawn_rejects_zero_parallel_without_waiting() {
    let store = Arc::new(MemoryTaskStore::default());
    let exec = TaskExecutor::new(Arc::clone(&store)).with_max_parallel(0);

    let result = time::timeout(
        Duration::from_millis(50),
        do_spawn(&exec, build_test_kernel(), req("u")),
    )
    .await
    .expect("zero parallel check should return before waiting");

    assert!(matches!(result, Err(TaskError::Executor(reason)) if reason.contains("max_parallel")));
}

#[tokio::test]
async fn finalize_task_marks_done() {
    let store = Arc::new(MemoryTaskStore::default());
    let r = req("u");
    let task = build_task_record(&r);
    let id = task.id.clone();
    store.insert(&task).await.expect("test result");
    finalize_task(&store, &id, TaskStatus::Done, None).await;
    let loaded = get_task(&store, &id).await;
    assert_eq!(loaded.status, TaskStatus::Done);
    assert!(loaded.finished_at.is_some());
}

#[tokio::test]
async fn finalize_task_marks_failed_with_error() {
    let store = Arc::new(MemoryTaskStore::default());
    let r = req("u");
    let task = build_task_record(&r);
    let id = task.id.clone();
    store.insert(&task).await.expect("test result");
    finalize_task(&store, &id, TaskStatus::Failed, Some("boom")).await;
    let loaded = get_task(&store, &id).await;
    assert_eq!(loaded.status, TaskStatus::Failed);
    assert_eq!(loaded.error.as_deref(), Some("boom"));
}

#[test]
fn build_run_request_subject_overrides_owner_default() {
    let r = req("u").with_subject(Subject::new("override"));
    let task = build_task_record(&r);
    let rr = build_run_request(
        &r,
        &task,
        CancellationToken::new(),
        Arc::new(OnceLock::new()),
    );
    assert_eq!(rr.subject.id(), "override");
}

#[tokio::test]
async fn cancel_unknown_task_id_returns_false() {
    let store = Arc::new(MemoryTaskStore::default());
    let exec = TaskExecutor::new(store);
    assert!(!do_cancel(&exec, &TaskId::new()).await);
}

struct CancelAwareProvider {
    started: std::sync::Mutex<Option<oneshot::Sender<()>>>,
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
        if let Some(tx) = started {
            let _receiver_dropped = tx.send(()).is_err();
        }
        cancel
            .expect("cancel token should be forwarded")
            .cancelled()
            .await;
        Err(ProviderError::Cancelled)
    }

    fn name(&self) -> &'static str {
        "cancel-aware"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("claude-haiku-4-5", "cancel-aware")]
    }
}

struct RecordingProvider {
    requests: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.requests
            .lock()
            .expect("requests mutex")
            .push(req.tools.iter().map(|tool| tool.name.clone()).collect());
        Ok(LlmResponse {
            text: "done".into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "recording"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("claude-haiku-4-5", "recording")]
    }
}

struct NamedTool(&'static str);

#[async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &'static str {
        self.0
    }

    fn description(&self) -> &'static str {
        "test tool"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(json!({"ok": true}))
    }
}

fn recording_kernel(requests: Arc<std::sync::Mutex<Vec<Vec<String>>>>) -> Arc<Kernel> {
    Arc::new(
        KernelBuilder::new()
            .provider(RecordingProvider { requests })
            .policy(AllowAllPolicy)
            .add_tool(NamedTool("task"))
            .add_tool(NamedTool("bash"))
            .build(),
    )
}

struct HistoryRecordingProvider {
    messages: Arc<std::sync::Mutex<Vec<Vec<Value>>>>,
}

#[async_trait]
impl Provider for HistoryRecordingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.messages
            .lock()
            .expect("messages mutex")
            .push(req.messages.clone());
        Ok(LlmResponse {
            text: "done".into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "history-recording"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("claude-haiku-4-5", "history-recording")]
    }
}

fn history_recording_kernel(messages: Arc<std::sync::Mutex<Vec<Vec<Value>>>>) -> Arc<Kernel> {
    Arc::new(
        KernelBuilder::new()
            .provider(HistoryRecordingProvider { messages })
            .policy(AllowAllPolicy)
            .add_tool(NamedTool("task"))
            .add_tool(NamedTool("bash"))
            .build(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_with_tool_access_none_advertises_no_tools() {
    let store = Arc::new(MemoryTaskStore::default());
    let exec = TaskExecutor::new(Arc::clone(&store));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kernel = recording_kernel(Arc::clone(&requests));

    exec.spawn_blocking(
        kernel,
        req("u").with_tool_access(ToolAccess::none()),
        Some(Duration::from_secs(1)),
    )
    .await
    .expect("task should finish");

    assert_eq!(
        requests.lock().expect("requests mutex").as_slice(),
        &[Vec::<String>::new()]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_with_tool_access_only_advertises_matching_tools() {
    let store = Arc::new(MemoryTaskStore::default());
    let exec = TaskExecutor::new(Arc::clone(&store));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kernel = recording_kernel(Arc::clone(&requests));

    exec.spawn_blocking(
        kernel,
        req("u").with_tool_access(ToolAccess::only(["task"])),
        Some(Duration::from_secs(1)),
    )
    .await
    .expect("task should finish");

    assert_eq!(
        requests.lock().expect("requests mutex").as_slice(),
        &[vec!["task".to_owned()]]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_with_tool_access_only_filters_context_tool_history() {
    let store = Arc::new(MemoryTaskStore::default());
    let exec = TaskExecutor::new(Arc::clone(&store));
    let messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kernel = history_recording_kernel(Arc::clone(&messages));
    let context = vec![
        user_message("start"),
        assistant_message(
            "",
            vec![tool_call("call-1", "bash"), tool_call("call-2", "task")],
        ),
        tool_result("call-1"),
        tool_result("call-2"),
    ];

    exec.spawn_blocking(
        kernel,
        req("u")
            .with_messages(context)
            .with_tool_access(ToolAccess::only(["task"])),
        Some(Duration::from_secs(1)),
    )
    .await
    .expect("task should finish");

    let captured = messages.lock().expect("messages mutex");
    let provider_messages = captured.first().expect("provider request");
    assert_eq!(provider_messages.len(), 3);
    assert_eq!(provider_messages[0]["role"], "user");
    assert_eq!(provider_messages[1]["role"], "assistant");
    assert_eq!(provider_messages[1]["tool_calls"][0]["name"], json!("task"));
    assert_eq!(
        provider_messages[1]["tool_calls"]
            .as_array()
            .expect("tool call array")
            .len(),
        1
    );
    assert_eq!(provider_messages[2]["role"], "tool_result");
    assert_eq!(provider_messages[2]["call_id"], "call-2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_aborts_running_task() {
    let store = Arc::new(MemoryTaskStore::default());
    let (started_tx, started_rx) = oneshot::channel();
    let kernel = cancel_kernel(started_tx);
    let exec = TaskExecutor::new(Arc::clone(&store))
        .with_timeout(Duration::from_mins(1))
        .with_shutdown_grace(Duration::from_millis(25));
    let id = do_spawn(&exec, kernel, req("u"))
        .await
        .expect("test result");
    started_rx.await.expect("test result");

    assert!(exec.cancel(&id).await);
    let task = wait_for_task_status(&store, &id, TaskStatus::Failed).await;
    assert_eq!(task.error.as_deref(), Some(CANCELLED_MESSAGE));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_blocking_returns_done_task() {
    let store = Arc::new(MemoryTaskStore::default());
    let exec = TaskExecutor::new(Arc::clone(&store));
    let task = do_spawn_blocking(&exec, build_test_kernel(), req("u"), None)
        .await
        .expect("test result");
    assert_eq!(task.status, TaskStatus::Done);
    assert_eq!(task.output, "done");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_blocking_returns_failed_on_timeout() {
    let store = Arc::new(MemoryTaskStore::default());
    let (started_tx, _started_rx) = oneshot::channel();
    let kernel = cancel_kernel(started_tx);
    let exec = TaskExecutor::new(Arc::clone(&store))
        .with_timeout(Duration::from_mins(1))
        .with_shutdown_grace(Duration::from_millis(25));
    let task = do_spawn_blocking(&exec, kernel, req("u"), Some(Duration::from_millis(10)))
        .await
        .expect("test result");
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(task.error.as_deref(), Some(TIMEOUT_MESSAGE));
}

struct GateProvider {
    started: mpsc::UnboundedSender<()>,
    release: CancellationToken,
}

#[async_trait]
impl Provider for GateProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let _receiver_dropped = self.started.send(()).is_err();
        self.release.cancelled().await;
        Ok(LlmResponse {
            text: "done".into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "gate"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("claude-haiku-4-5", "gate")]
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_max_parallel_one_blocks_second_spawn_until_first_completes() {
    let store = Arc::new(MemoryTaskStore::default());
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let release = CancellationToken::new();
    let kernel = Arc::new(
        KernelBuilder::new()
            .provider(GateProvider {
                started: started_tx,
                release: release.clone(),
            })
            .policy(AllowAllPolicy)
            .build(),
    );
    let exec = Arc::new(TaskExecutor::new(Arc::clone(&store)).with_max_parallel(1));
    let first = exec
        .spawn(Arc::clone(&kernel), req("first"))
        .await
        .expect("first spawn");
    started_rx.recv().await.expect("first task should start");

    let mut second = tokio::spawn({
        let exec = Arc::clone(&exec);
        let kernel = Arc::clone(&kernel);
        async move { exec.spawn(kernel, req("second")).await }
    });
    time::timeout(Duration::from_millis(30), &mut second)
        .await
        .expect_err("expected error");
    release.cancel();
    let second = time::timeout(Duration::from_secs(1), second)
        .await
        .expect("second spawn should unblock")
        .expect("second join")
        .expect("second spawn result");
    wait_for_task_status(&store, &first, TaskStatus::Done).await;
    wait_for_task_status(&store, &second, TaskStatus::Done).await;
}

async fn wait_for_task_status(
    store: &Arc<MemoryTaskStore>,
    id: &TaskId,
    status: TaskStatus,
) -> Task {
    let deadline = time::Instant::now() + Duration::from_secs(2);
    loop {
        let task = get_task(store, id).await;
        if task.status == status {
            return task;
        }
        assert!(
            time::Instant::now() < deadline,
            "task did not reach expected status"
        );
        time::sleep(Duration::from_millis(10)).await;
    }
}
