use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    AllowAllPolicy, CancelReason, Decision, Hook, Kernel, KernelError, LlmRequest, LlmResponse,
    ModelInfo, ModelTarget, Outcome, Provider, ProviderCapabilities, ProviderError, RunCtx, RunId,
    RunRequest, Subject, Tool, ToolCall, ToolCtx, ToolError, ToolResult,
};
use crabgent_test_support::{done, tool_call, tool_use, user_msg};

type ProbeLog = Vec<(Outcome, Option<CancelReason>)>;

#[derive(Debug, Clone)]
struct Probe(Arc<Mutex<ProbeLog>>);

impl Probe {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    async fn snapshot(&self) -> ProbeLog {
        self.0.lock().await.clone()
    }
}

struct ProbeOnStopHook(Probe);

#[async_trait]
impl Hook for ProbeOnStopHook {
    async fn on_stop(&self, ctx: &RunCtx, outcome: &Outcome) {
        self.0
            .0
            .lock()
            .await
            .push((outcome.clone(), ctx.cancel_reason()));
    }
}

struct CancelAfterToolHook;

#[async_trait]
impl Hook for CancelAfterToolHook {
    async fn after_tool(
        &self,
        _call: &ToolCall,
        _result: &ToolResult,
        ctx: &RunCtx,
    ) -> Decision<ToolResult> {
        ctx.set_cancel_reason(CancelReason::Hook)
            .expect("first reason set should succeed");
        ctx.cancel.cancel();
        Decision::Continue
    }
}

struct CancelBeforeLlmHook;

#[async_trait]
impl Hook for CancelBeforeLlmHook {
    async fn before_llm(&self, _req: &LlmRequest, ctx: &RunCtx) -> Decision<LlmRequest> {
        ctx.set_cancel_reason(CancelReason::Hook)
            .expect("first reason set should succeed");
        ctx.cancel.cancel();
        Decision::Continue
    }
}

struct ScriptedProvider {
    responses: Mutex<Vec<LlmResponse>>,
    calls: Arc<AtomicUsize>,
}

impl ScriptedProvider {
    fn with(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.calls)
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut responses = self.responses.lock().await;
        if responses.is_empty() {
            return Err(ProviderError::Other("script exhausted".into()));
        }
        Ok(responses.remove(0))
    }

    fn name(&self) -> &'static str {
        "scripted"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("m", "scripted")]
    }
}

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "echo for hook-cancel test"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }
    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(json!({"ok": true}))
    }
}

struct CancelledTool;

#[async_trait]
impl Tool for CancelledTool {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "cancelled tool for hook-cancel test"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }
    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Err(ToolError::Cancelled)
    }
}

fn tool_use_response(call_id: &str) -> LlmResponse {
    tool_use(vec![tool_call(call_id, "echo", json!({}))])
}

fn done_response(text: &str) -> LlmResponse {
    done(text)
}

fn run_request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("u"),
        model: ModelTarget::id("m"),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![user_msg("hi")],
        system_prompt: None,
        max_turns: Some(5),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

#[tokio::test]
async fn hook_cancels_run_from_after_tool_via_ctx_cancel() {
    let probe = Probe::new();
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::with(vec![
            tool_use_response("call-1"),
            done_response("never-reached"),
        ]))
        .add_tool(EchoTool)
        .add_hook(CancelAfterToolHook)
        .add_hook(ProbeOnStopHook(probe.clone()))
        .policy(AllowAllPolicy)
        .build();

    let result = kernel.run(run_request(), None).await;

    assert!(
        matches!(result, Err(KernelError::Cancelled)),
        "after_tool hook cancel should surface Cancelled, got {result:?}"
    );
    let observed = probe.snapshot().await;
    assert_eq!(observed.len(), 1, "on_stop should fire exactly once");
    let (outcome, reason) = &observed[0];
    assert!(
        matches!(outcome, Outcome::Cancelled),
        "outcome should be Cancelled, got {outcome:?}"
    );
    assert_eq!(*reason, Some(CancelReason::Hook));
}

#[tokio::test]
async fn tool_cancelled_error_stops_with_cancelled_outcome() {
    let probe = Probe::new();
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::with(vec![tool_use_response("call-1")]))
        .add_tool(CancelledTool)
        .add_hook(ProbeOnStopHook(probe.clone()))
        .policy(AllowAllPolicy)
        .build();

    let result = kernel.run(run_request(), None).await;

    assert!(
        matches!(result, Err(KernelError::Cancelled)),
        "tool cancellation should surface as kernel cancellation, got {result:?}"
    );
    let observed = probe.snapshot().await;
    assert_eq!(observed.len(), 1, "on_stop should fire exactly once");
    let (outcome, reason) = &observed[0];
    assert!(
        matches!(outcome, Outcome::Cancelled),
        "outcome should be Cancelled, got {outcome:?}"
    );
    assert_eq!(*reason, None);
}

#[tokio::test]
async fn hook_cancel_from_before_llm_stops_next_provider_call() {
    let provider = ScriptedProvider::with(vec![done_response("never-reached")]);
    let calls = provider.calls();
    let probe = Probe::new();
    let kernel = Kernel::builder()
        .provider(provider)
        .add_hook(CancelBeforeLlmHook)
        .add_hook(ProbeOnStopHook(probe.clone()))
        .policy(AllowAllPolicy)
        .build();

    let result = kernel.run(run_request(), None).await;

    assert!(
        matches!(result, Err(KernelError::Cancelled)),
        "before_llm hook cancel should surface Cancelled, got {result:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "provider.complete must not be called when before_llm cancels",
    );
    let observed = probe.snapshot().await;
    assert_eq!(observed.len(), 1, "on_stop should fire exactly once");
    let (outcome, reason) = &observed[0];
    assert!(
        matches!(outcome, Outcome::Cancelled),
        "outcome should be Cancelled, got {outcome:?}"
    );
    assert_eq!(*reason, Some(CancelReason::Hook));
}

#[tokio::test]
async fn default_run_carries_unset_cancel_reason() {
    let probe = Probe::new();
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::with(vec![done_response("ok")]))
        .add_hook(ProbeOnStopHook(probe.clone()))
        .policy(AllowAllPolicy)
        .build();

    let text = kernel.run(run_request(), None).await.expect("run succeeds");
    assert_eq!(text, "ok");

    let observed = probe.snapshot().await;
    assert_eq!(observed.len(), 1, "on_stop should fire exactly once");
    let (outcome, reason) = &observed[0];
    assert!(
        matches!(outcome, Outcome::Completed(_)),
        "outcome should be Completed, got {outcome:?}"
    );
    assert!(
        reason.is_none(),
        "cancel_reason should stay unset for non-cancelled runs, got {reason:?}",
    );
}
