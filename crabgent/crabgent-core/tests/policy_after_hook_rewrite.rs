mod common;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

use common::{NoopTool, RecordingProvider, calling_tool, done};
use crabgent_core::{
    Action, ContentBlock, Decision, Event, Hook, Kernel, KernelError, LlmRequest, LlmResponse,
    Message, ModelInfo, PolicyDecision, PolicyHook, Provider, ProviderCapabilities, ProviderError,
    RunCtx, RunId, RunRequest, StopReason, Subject, Tool, ToolCall, ToolCtx, ToolError, Usage,
};
use tokio_util::sync::CancellationToken;

struct AllowOnlyAdvertisedTool;

#[async_trait]
impl PolicyHook for AllowOnlyAdvertisedTool {
    async fn allow(&self, _subject: &Subject, action: &Action) -> PolicyDecision {
        match action {
            Action::LlmCall => PolicyDecision::Allow,
            Action::ToolCall(name) if name == "advertised" => PolicyDecision::Allow,
            Action::ToolCall(name) => PolicyDecision::Deny(format!("tool {name} denied")),
            other => PolicyDecision::Deny(format!("unexpected action {}", other.name())),
        }
    }
}

struct RewriteToForbiddenTool;

#[async_trait]
impl Hook for RewriteToForbiddenTool {
    async fn before_tool(&self, call: &ToolCall, _ctx: &RunCtx) -> Decision<ToolCall> {
        let mut rewritten = call.clone();
        rewritten.name = "forbidden".into();
        Decision::Replace(rewritten)
    }
}

struct EnableWebSearchHook;

#[async_trait]
impl Hook for EnableWebSearchHook {
    async fn before_llm(&self, req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        let mut req = req.clone();
        req.web_search.enabled = true;
        Decision::Replace(req)
    }
}

struct DenyHostedWebSearchPolicy;

#[async_trait]
impl PolicyHook for DenyHostedWebSearchPolicy {
    async fn allow(&self, _subject: &Subject, action: &Action) -> PolicyDecision {
        match action {
            Action::LlmCall => PolicyDecision::Allow,
            Action::HostedWebSearch { provider } => {
                PolicyDecision::Deny(format!("web search denied for {provider}"))
            }
            _ => PolicyDecision::Deny(format!("unexpected action {}", action.name())),
        }
    }
}

struct WebSearchProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for WebSearchProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            text: "should not run".into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "web-provider"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            web_search: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        let mut info = ModelInfo::minimal("m", "web-provider");
        info.caps.supports_web_search = true;
        vec![info]
    }
}

struct CountingTool {
    name: &'static str,
    calls: Arc<AtomicUsize>,
}

impl CountingTool {
    const fn new(name: &'static str, calls: Arc<AtomicUsize>) -> Self {
        Self { name, calls }
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        "test tool"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"ok": true}))
    }
}

fn request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("policy-test"),
        model: "m".into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text { text: "hi".into() }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(2),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

fn kernel(requests: Arc<Mutex<Vec<LlmRequest>>>, executed: Arc<AtomicUsize>) -> Kernel {
    Kernel::builder()
        .provider(RecordingProvider::with(
            vec![
                calling_tool("advertised", "call-1", json!({})),
                done("repaired after deny"),
            ],
            requests,
        ))
        .policy(AllowOnlyAdvertisedTool)
        .add_tool(NoopTool)
        .add_tool(CountingTool::new(
            "advertised",
            Arc::new(AtomicUsize::new(0)),
        ))
        .add_tool(CountingTool::new("forbidden", executed))
        .add_hook(RewriteToForbiddenTool)
        .build()
}

fn assert_run_soft_denied_recorded_request(
    requests: &[LlmRequest],
    call_id: &str,
    expected_reason_fragment: &str,
) {
    let second_req = requests
        .get(1)
        .expect("second LlmRequest records tool_result after soft deny");
    let tool_result_msg = second_req
        .messages
        .iter()
        .find(|m| {
            m.get("role").and_then(Value::as_str) == Some("tool_result")
                && m.get("call_id").and_then(Value::as_str) == Some(call_id)
        })
        .expect("tool_result for call_id present in second LlmRequest");
    assert_eq!(tool_result_msg.get("is_error"), Some(&json!(true)));
    let output = tool_result_msg
        .get("output")
        .and_then(Value::as_str)
        .expect("output is a string");
    assert!(
        output.contains(expected_reason_fragment),
        "expected reason '{expected_reason_fragment}' in '{output}'"
    );
}

#[tokio::test]
async fn run_rechecks_policy_after_hook_rewrites_tool_call() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let forbidden_calls = Arc::new(AtomicUsize::new(0));
    let kernel = kernel(requests.clone(), forbidden_calls.clone());

    let text = kernel.run(request(), None).await.expect("run continues");

    assert_eq!(text, "repaired after deny");
    assert_run_soft_denied_recorded_request(
        &requests.lock().expect("requests mutex not poisoned"),
        "call-1",
        "tool forbidden denied",
    );
    assert_eq!(forbidden_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn streaming_rechecks_policy_after_hook_rewrites_tool_call() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let forbidden_calls = Arc::new(AtomicUsize::new(0));
    let kernel = kernel(requests, forbidden_calls.clone());
    let stream = kernel.run_streaming(request(), None);
    tokio::pin!(stream);

    let mut saw_started_forbidden = false;
    let mut saw_completed_soft_error = false;
    let mut final_text = None;

    while let Some(item) = stream.next().await {
        match item.expect("stream item") {
            Event::ToolCallStarted(call) if call.name == "forbidden" => {
                saw_started_forbidden = true;
            }
            Event::ToolCallCompleted { result, .. } if result.is_error => {
                saw_completed_soft_error = true;
            }
            Event::Final(text) => {
                final_text = Some(text);
            }
            _ => {}
        }
    }

    assert!(saw_started_forbidden);
    assert!(saw_completed_soft_error);
    assert_eq!(final_text.as_deref(), Some("repaired after deny"));
    assert_eq!(forbidden_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn before_llm_web_search_rewrite_is_policy_checked_before_provider_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::builder()
        .provider(WebSearchProvider {
            calls: Arc::clone(&calls),
        })
        .policy(DenyHostedWebSearchPolicy)
        .add_hook(EnableWebSearchHook)
        .build();

    let err = kernel
        .run(request(), None)
        .await
        .expect_err("web search rewrite should be denied");

    assert!(matches!(
        err,
        KernelError::PolicyDenied { reason } if reason == "web search denied for web-provider"
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
