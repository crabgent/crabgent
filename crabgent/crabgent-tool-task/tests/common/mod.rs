#![allow(
    dead_code,
    reason = "shared integration-test helpers are used per test target"
)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_core::{
    Action, AllowAllPolicy, EffortSource, Kernel, KernelBuilder, LlmRequest, LlmResponse,
    ModelInfo, Owner, PolicyDecision, PolicyHook, Provider, ProviderCapabilities, ProviderError,
    ReasoningEffort, ResolvedEffort, RunCtx, StopReason, Subject, Tool, ToolCtx, ToolError, Usage,
};
use crabgent_core::{ResolvedModelWithSource, ResolvedSource};
use crabgent_store::TaskStore;
use crabgent_store::{MemoryTaskStore, Page, SessionId, StoreError, Task, TaskId, TaskStatus};
use crabgent_task::{TaskExecutor, TaskRequest};
use crabgent_tool_task::TaskTool;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken; // test-helper

pub const MODEL: &str = "stub-model";

pub struct Harness<S: TaskStore + 'static> {
    pub tool: TaskTool<S>,
    pub store: Arc<S>,
    pub executor: Arc<TaskExecutor<S>>,
}

pub fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("alice"))
}

pub fn ctx_with_current_model(source: ResolvedSource) -> ToolCtx {
    ToolCtx::new(Subject::new("alice")).with_current_model(ResolvedModelWithSource {
        info: vision_model("stub"),
        source,
    })
}

pub fn ctx_with_current_model_and_effort(
    source: ResolvedSource,
    effort: ReasoningEffort,
) -> ToolCtx {
    ctx_with_current_model(source).with_current_effort(ResolvedEffort {
        effort: Some(effort),
        source: EffortSource::SessionOverride,
    })
}

pub fn ctx_with_parent(parent: &TaskId) -> ToolCtx {
    ToolCtx::new(Subject::new("alice").with_attr("parent_task_id", parent.to_string()))
}

pub fn create_args(prompt: &str) -> Value {
    json!({
        "op": "create",
        "prompt": prompt,
        "model": MODEL
    })
}

pub fn id_args(op: &str, id: &TaskId) -> Value {
    json!({
        "op": op,
        "task_id": id.to_string()
    })
}

pub fn build_immediate_harness() -> Harness<MemoryTaskStore> {
    build_harness(
        ImmediateProvider::new("done"),
        TaskExecutor::new,
        Arc::new(AllowAllPolicy),
    )
}

pub fn build_harness<P, F>(
    provider: P,
    build_executor: F,
    policy: Arc<dyn PolicyHook>,
) -> Harness<MemoryTaskStore>
where
    P: Provider + 'static,
    F: FnOnce(Arc<MemoryTaskStore>) -> TaskExecutor<MemoryTaskStore>,
{
    let store = Arc::new(MemoryTaskStore::default());
    let executor = Arc::new(build_executor(Arc::clone(&store)));
    let kernel = Arc::new(
        KernelBuilder::new()
            .provider(provider)
            .policy(AllowAllPolicy)
            .build(),
    );
    let tool = TaskTool::new(Arc::clone(&executor), kernel, Arc::clone(&store), policy);
    Harness {
        tool,
        store,
        executor,
    }
}

pub fn build_harness_with_named_tools<P, F>(
    provider: P,
    build_executor: F,
    policy: Arc<dyn PolicyHook>,
    tool_names: &[&'static str],
) -> Harness<MemoryTaskStore>
where
    P: Provider + 'static,
    F: FnOnce(Arc<MemoryTaskStore>) -> TaskExecutor<MemoryTaskStore>,
{
    let store = Arc::new(MemoryTaskStore::default());
    let executor = Arc::new(build_executor(Arc::clone(&store)));
    let mut builder = KernelBuilder::new()
        .provider(provider)
        .policy(AllowAllPolicy);
    for name in tool_names {
        builder = builder.add_tool(NamedTool(name));
    }
    let tool = TaskTool::new(
        Arc::clone(&executor),
        Arc::new(builder.build()),
        Arc::clone(&store),
        policy,
    );
    Harness {
        tool,
        store,
        executor,
    }
}

pub fn build_harness_for_store<S, P>(
    store: Arc<S>,
    provider: P,
    policy: Arc<dyn PolicyHook>,
) -> Harness<S>
where
    S: TaskStore + 'static,
    P: Provider + 'static,
{
    let executor = Arc::new(TaskExecutor::new(Arc::clone(&store)));
    let kernel = Arc::new(
        KernelBuilder::new()
            .provider(provider)
            .policy(AllowAllPolicy)
            .build(),
    );
    let tool = TaskTool::new(Arc::clone(&executor), kernel, Arc::clone(&store), policy);
    Harness {
        tool,
        store,
        executor,
    }
}

pub fn build_kernel<P>(provider: P) -> Arc<Kernel>
where
    P: Provider + 'static,
{
    Arc::new(
        KernelBuilder::new()
            .provider(provider)
            .policy(AllowAllPolicy)
            .build(),
    )
}

#[derive(Clone)]
pub struct ImmediateProvider {
    text: &'static str,
    seen: Option<Arc<Mutex<Vec<LlmRequest>>>>,
    supports_reasoning: bool,
}

impl ImmediateProvider {
    pub const fn new(text: &'static str) -> Self {
        Self {
            text,
            seen: None,
            supports_reasoning: true,
        }
    }

    pub const fn recording(text: &'static str, seen: Arc<Mutex<Vec<LlmRequest>>>) -> Self {
        Self {
            text,
            seen: Some(seen),
            supports_reasoning: true,
        }
    }

    pub const fn recording_without_reasoning(
        text: &'static str,
        seen: Arc<Mutex<Vec<LlmRequest>>>,
    ) -> Self {
        Self {
            text,
            seen: Some(seen),
            supports_reasoning: false,
        }
    }
}

#[async_trait]
impl Provider for ImmediateProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        if let Some(seen) = &self.seen {
            seen.lock().expect("seen lock").push(req.clone());
        }
        Ok(LlmResponse {
            text: self.text.to_owned(),
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
            vision: true,
            tools: true,
            system_prompt: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        let mut info = vision_model(self.name());
        if !self.supports_reasoning {
            info.caps.reasoning_effort = None;
        }
        vec![info]
    }
}

pub struct HangingProvider {
    started: Mutex<Option<oneshot::Sender<()>>>,
    cancel_delay: Duration,
}

impl HangingProvider {
    pub fn new() -> (Self, oneshot::Receiver<()>) {
        Self::with_cancel_delay(Duration::ZERO)
    }

    pub fn with_cancel_delay(cancel_delay: Duration) -> (Self, oneshot::Receiver<()>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                started: Mutex::new(Some(tx)),
                cancel_delay,
            },
            rx,
        )
    }
}

#[async_trait]
impl Provider for HangingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let started = self.started.lock().expect("started lock").take();
        if let Some(started) = started
            && started.send(()).is_err()
        {
            // Receiver was dropped by the test after observing enough state.
        }
        if let Some(cancel) = cancel {
            cancel.cancelled().await;
            if !self.cancel_delay.is_zero() {
                tokio::time::sleep(self.cancel_delay).await;
            }
        } else {
            std::future::pending::<()>().await;
        }
        Ok(LlmResponse {
            text: "cancelled".to_owned(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "hanging"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            vision: true,
            system_prompt: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![vision_model(self.name())]
    }
}

fn vision_model(provider: &str) -> ModelInfo {
    let mut info = ModelInfo::minimal(MODEL, provider);
    info.caps.supports_vision = true;
    info.caps.reasoning_effort = Some(ReasoningEffort::Low);
    info
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

pub struct DenyNamedPolicy {
    denied: HashSet<&'static str>,
}

impl DenyNamedPolicy {
    pub fn new(names: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            denied: names.into_iter().collect(),
        }
    }
}

#[async_trait]
impl PolicyHook for DenyNamedPolicy {
    async fn allow(&self, _subject: &Subject, action: &Action) -> PolicyDecision {
        if self.denied.contains(action.name()) {
            PolicyDecision::Deny(format!("denied {}", action.name()))
        } else {
            PolicyDecision::Allow
        }
    }
}

pub fn test_task(owner: &str, status: TaskStatus, parent_task_id: Option<TaskId>) -> Task {
    test_task_with_id(TaskId::new(), owner, status, parent_task_id)
}

pub fn test_task_with_id(
    id: TaskId,
    owner: &str,
    status: TaskStatus,
    parent_task_id: Option<TaskId>,
) -> Task {
    let now = Utc::now();
    let finished_at = if matches!(status, TaskStatus::Running) {
        None
    } else {
        Some(now)
    };
    Task {
        resume_spec: None,
        resume_count: 0,
        pause_cause: None,
        paused_at: None,
        id,
        owner: Owner::new(owner),
        name: None,
        prompt: format!("prompt for {owner}"),
        status,
        output: if matches!(status, TaskStatus::Done) {
            "stored output".to_owned()
        } else {
            String::new()
        },
        error: if matches!(status, TaskStatus::Failed) {
            Some("stored error".to_owned())
        } else {
            None
        },
        created_at: now,
        updated_at: now,
        finished_at,
        parent_session_id: None,
        parent_task_id,
        context_mode: None,
        reasoning_effort_override: None,
    }
}

pub async fn insert_task(store: &MemoryTaskStore, task: Task) -> Task {
    store.insert(&task).await.expect("insert task");
    task
}

pub async fn load_task(store: &MemoryTaskStore, id: &TaskId) -> Task {
    store
        .get(id)
        .await
        .expect("load task")
        .expect("task exists")
}

pub fn task_request(prompt: &str) -> TaskRequest {
    TaskRequest::new(Owner::new("alice"), MODEL, prompt)
}

pub fn session_id_string() -> String {
    SessionId::new().to_string()
}

pub fn assert_permission(err: crabgent_core::ToolError, contains: &str) {
    let reason = permission_reason(err).expect("expected permission error");
    assert!(
        reason.contains(contains),
        "unexpected permission reason: {reason}"
    );
}

fn permission_reason(err: crabgent_core::ToolError) -> Option<String> {
    match err {
        crabgent_core::ToolError::Permission(reason) => Some(reason),
        _ => None,
    }
}

#[derive(Default)]
pub struct FailingStore;

#[async_trait]
impl TaskStore for FailingStore {
    async fn insert(&self, _task: &Task) -> Result<(), StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn get(&self, _id: &TaskId) -> Result<Option<Task>, StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn append_output(&self, _id: &TaskId, _chunk: &str) -> Result<(), StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn finish(
        &self,
        _id: &TaskId,
        _status: TaskStatus,
        _error: Option<&str>,
    ) -> Result<(), StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn list_running(&self, _page: Page) -> Result<Vec<Task>, StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn pause(
        &self,
        _id: &TaskId,
        _cause: crabgent_store::TaskPauseCause,
    ) -> Result<bool, StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn claim_for_resume(&self, _id: &TaskId, _max_resumes: u32) -> Result<bool, StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn list_paused(&self, _page: Page) -> Result<Vec<Task>, StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn pause_orphans(&self, _stale_secs: i64) -> Result<Vec<TaskId>, StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn list_children(&self, _parent: &TaskId, _page: Page) -> Result<Vec<Task>, StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn save_transcript(
        &self,
        _id: &TaskId,
        _messages: &[crabgent_core::Message],
    ) -> Result<(), StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn load_transcript(
        &self,
        _id: &TaskId,
    ) -> Result<Option<Vec<crabgent_core::Message>>, StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn recover_stuck(&self, _timeout_secs: i64) -> Result<Vec<TaskId>, StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }

    async fn cleanup_old(&self, _days: i64) -> Result<u64, StoreError> {
        Err(StoreError::backend("dsn=postgres://secret@example/tasks"))
    }
}
