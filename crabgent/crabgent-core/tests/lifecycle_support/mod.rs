//! Shared test doubles and helpers for the run-lifecycle integration tests.
//!
//! Split across two test binaries (`run_lifecycle`, `run_outcomes`); each one
//! uses a subset, so some items look unused per binary.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    ContentBlock, Decision, Event, Hook, KernelError, LlmRequest, LlmResponse, Message, ModelInfo,
    Outcome, Provider, ProviderCapabilities, ProviderError, RunCtx, RunId, RunRequest, Subject,
    Tool, ToolCtx, ToolError,
};
pub use crabgent_test_support::done;
use crabgent_test_support::{StubProvider, tool_call, tool_use};

pub type Trace = Arc<Mutex<Vec<String>>>;

pub struct TraceHook {
    trace: Trace,
}

impl TraceHook {
    pub const fn new(trace: Trace) -> Self {
        Self { trace }
    }

    fn push(&self, event: impl Into<String>) {
        self.trace
            .lock()
            .expect("mutex should not be poisoned")
            .push(event.into());
    }
}

#[async_trait]
impl Hook for TraceHook {
    async fn on_session_start(&self, _ctx: &RunCtx) -> Decision<()> {
        self.push("session_start");
        Decision::Continue
    }

    async fn on_user_prompt_submit(
        &self,
        _msgs: &[Message],
        _ctx: &RunCtx,
    ) -> Decision<Vec<Message>> {
        self.push("user_prompt");
        Decision::Continue
    }

    async fn on_message(&self, msgs: &[Message], _ctx: &RunCtx) -> Decision<Vec<Message>> {
        self.push(format!("on_message:{}", msgs.len()));
        Decision::Continue
    }

    async fn before_llm(&self, _req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        self.push("before_llm");
        Decision::Continue
    }

    async fn after_llm(
        &self,
        _req: &LlmRequest,
        _resp: &LlmResponse,
        _ctx: &RunCtx,
    ) -> Decision<LlmResponse> {
        self.push("after_llm");
        Decision::Continue
    }

    async fn on_event(&self, ev: &Event, _ctx: &RunCtx) -> Decision<Event> {
        self.push(match ev {
            Event::Token(_) => "event:token",
            Event::ToolCallStarted(_) => "event:tool_started",
            Event::ToolCallCompleted { .. } => "event:tool_completed",
            Event::Notification(_) => "event:notification",
            Event::Final(_) => "event:final",
            _ => "event:unknown",
        });
        Decision::Continue
    }

    async fn on_error(&self, _ctx: &RunCtx, err: &KernelError) {
        self.push(format!("on_error:{err}"));
    }

    async fn on_stop(&self, _ctx: &RunCtx, outcome: &Outcome) {
        self.push(outcome_label(outcome));
    }
}

pub struct ReplaceAssistantHook;

#[async_trait]
impl Hook for ReplaceAssistantHook {
    async fn on_message(&self, msgs: &[Message], _ctx: &RunCtx) -> Decision<Vec<Message>> {
        let Some(Message::Assistant { text, .. }) = msgs.last() else {
            return Decision::Continue;
        };
        if text != "original" {
            return Decision::Continue;
        }

        let mut replaced = msgs.to_vec();
        if let Some(Message::Assistant { text, .. }) = replaced.last_mut() {
            *text = "replaced".into();
        }
        Decision::Replace(replaced)
    }
}

pub struct CaptureMessageHook {
    seen: Arc<Mutex<Vec<Message>>>,
}

impl CaptureMessageHook {
    pub const fn new(seen: Arc<Mutex<Vec<Message>>>) -> Self {
        Self { seen }
    }
}

#[async_trait]
impl Hook for CaptureMessageHook {
    async fn on_message(&self, msgs: &[Message], _ctx: &RunCtx) -> Decision<Vec<Message>> {
        *self.seen.lock().expect("mutex should not be poisoned") = msgs.to_vec();
        Decision::Continue
    }
}

pub struct DenyToolResultHook;

#[async_trait]
impl Hook for DenyToolResultHook {
    async fn on_message(&self, msgs: &[Message], _ctx: &RunCtx) -> Decision<Vec<Message>> {
        if matches!(msgs.last(), Some(Message::ToolResult { .. })) {
            return Decision::Deny("tool result denied".into());
        }
        Decision::Continue
    }
}

pub fn outcome_label(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Completed(text) => format!("on_stop:completed:{text}"),
        Outcome::MaxTurnsExceeded => "on_stop:max_turns".into(),
        Outcome::Cancelled => "on_stop:cancelled".into(),
        Outcome::Paused => "on_stop:paused".into(),
        Outcome::Errored(reason) => format!("on_stop:errored:{reason}"),
        _ => "on_stop:unknown".into(),
    }
}

/// Scripted tool-capable provider: one response consumed per `complete` call.
pub fn scripted(responses: Vec<LlmResponse>) -> StubProvider {
    StubProvider::new()
        .responses(responses)
        .with_tools(true)
        .with_models(vec![ModelInfo::minimal("m", "stub")])
}

/// Tool-capable provider that fails every call with `ProviderError::Other`.
pub fn error_provider() -> StubProvider {
    StubProvider::new()
        .with_tools(true)
        .with_models(vec![ModelInfo::minimal("m", "stub")])
        .fail_with(|| ProviderError::Other("boom".into()))
}

pub struct PendingProvider;

#[async_trait]
impl Provider for PendingProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Other(
            "pending provider only supports stream".into(),
        ))
    }

    async fn stream(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<crabgent_core::provider::EventStream, ProviderError> {
        Ok(Box::pin(stream::pending()))
    }

    fn name(&self) -> &'static str {
        "pending"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("m", "pending")]
    }
}

pub struct NoopTool;

#[async_trait]
impl Tool for NoopTool {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn description(&self) -> &'static str {
        "test noop"
    }

    fn parameters_schema(&self) -> Value {
        json!({})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(json!({"ok": true}))
    }
}

pub fn calling_tool() -> LlmResponse {
    tool_use(vec![tool_call("c1", "noop", json!({}))])
}

pub fn request(max_turns: u32) -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("u"),
        model: "m".into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text { text: "hi".into() }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(max_turns),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

pub fn trace_snapshot(trace: &Trace) -> Vec<String> {
    trace.lock().expect("mutex should not be poisoned").clone()
}

pub async fn wait_for_trace(trace: &Trace, expected: &str) {
    let expected = expected.to_owned();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if trace_snapshot(trace).contains(&expected) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_elapsed| {
        panic!(
            "trace did not contain {expected}: {:?}",
            trace_snapshot(trace)
        )
    });
}
