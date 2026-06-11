use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, KernelError, LlmRequest, LlmResponse, Message, ModelInfo,
    Provider, ProviderCapabilities, ProviderError, RunCtx, RunId, RunRequest, Subject, Tool,
    ToolCtx, ToolError, ToolResult,
};
use crabgent_test_support::{done, tool_call, tool_use};

#[path = "common/noop_tool.rs"]
mod noop_tool;

use noop_tool::NoopTool;

struct RecordingProvider {
    responses: Mutex<Vec<LlmResponse>>,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl RecordingProvider {
    const fn with(responses: Vec<LlmResponse>, requests: Arc<Mutex<Vec<LlmRequest>>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            requests,
        }
    }
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
            .expect("mutex should not be poisoned")
            .push(req.clone());
        let mut q = self.responses.lock().expect("mutex should not be poisoned");
        if q.is_empty() {
            return Err(ProviderError::Other("script exhausted".into()));
        }
        Ok(q.remove(0))
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
        vec![ModelInfo::minimal("test", "recording")]
    }
}

struct SoftErrorTool;

#[async_trait]
impl Tool for SoftErrorTool {
    fn name(&self) -> &'static str {
        "soft_error"
    }

    fn description(&self) -> &'static str {
        "recoverable validation failure"
    }

    fn parameters_schema(&self) -> Value {
        json!({})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Err(ToolError::Execution(
            "SoftErrorTool uses execute_result".into(),
        ))
    }

    async fn execute_result(&self, _args: Value, _ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::soft_error(json!(
            "validation failed: empty input"
        )))
    }
}

struct HardErrorTool;

#[async_trait]
impl Tool for HardErrorTool {
    fn name(&self) -> &'static str {
        "hard_error"
    }

    fn description(&self) -> &'static str {
        "unrecoverable failure"
    }

    fn parameters_schema(&self) -> Value {
        json!({})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Err(ToolError::Execution("hard failure".into()))
    }
}

fn calling_tool(name: &str, id: &str, args: Value) -> LlmResponse {
    tool_use(vec![tool_call(id, name, args)])
}

fn make_request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("integration-user"),
        model: "test".into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
            timestamp: None,
        }],
        system_prompt: Some("you are testing".into()),
        max_turns: Some(5),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

fn recorded_tool_result<'a>(req: &'a LlmRequest, call_id: &str) -> &'a Value {
    req.messages
        .iter()
        .find(|msg| {
            msg.get("role").and_then(Value::as_str) == Some("tool_result")
                && msg.get("call_id").and_then(Value::as_str) == Some(call_id)
        })
        .expect("tool result message")
}

#[tokio::test]
async fn tool_success_sends_is_error_false_to_next_provider_turn() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(RecordingProvider::with(
            vec![calling_tool("noop", "c1", json!({})), done("done")],
            requests.clone(),
        ))
        .policy(AllowAllPolicy)
        .add_tool(NoopTool)
        .build();

    let text = kernel.run(make_request(), None).await.expect("ok");
    assert_eq!(text, "done");

    let recorded = requests.lock().expect("mutex should not be poisoned");
    let result = recorded_tool_result(
        recorded
            .get(1)
            .expect("second provider request should contain tool result"),
        "c1",
    );
    // Hardening design8 Layer 2: every tool result is wrapped in boundary tags at
    // the kernel sink. The raw NoopTool output `{}` is stringified and
    // wrapped before reaching the next provider turn.
    assert_eq!(
        result.get("output"),
        Some(&json!("<tool_output tool=\"noop\">{}</tool_output>"))
    );
    assert_eq!(result.get("is_error"), Some(&json!(false)));
}

#[tokio::test]
async fn tool_soft_error_sends_is_error_true_and_run_continues() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(RecordingProvider::with(
            vec![
                calling_tool("soft_error", "c1", json!({})),
                done("repaired"),
            ],
            requests.clone(),
        ))
        .policy(AllowAllPolicy)
        .add_tool(SoftErrorTool)
        .build();

    let text = kernel.run(make_request(), None).await.expect("ok");
    assert_eq!(text, "repaired");

    let recorded = requests.lock().expect("mutex should not be poisoned");
    let result = recorded_tool_result(
        recorded
            .get(1)
            .expect("second provider request should contain tool result"),
        "c1",
    );
    // Hardening design8 Layer 2: soft-error output is wrapped in `<tool_error>`
    // boundary tags at the kernel sink before reaching the next turn.
    assert_eq!(
        result.get("output"),
        Some(&json!(
            "<tool_error tool=\"soft_error\">validation failed: empty input</tool_error>"
        ))
    );
    assert_eq!(result.get("is_error"), Some(&json!(true)));
}

#[tokio::test]
async fn tool_hard_error_stops_run() {
    let kernel = Kernel::builder()
        .provider(RecordingProvider::with(
            vec![calling_tool("hard_error", "c1", json!({}))],
            Arc::new(Mutex::new(Vec::new())),
        ))
        .policy(AllowAllPolicy)
        .add_tool(HardErrorTool)
        .build();

    let err = kernel
        .run(make_request(), None)
        .await
        .expect_err("hard tool error");
    assert!(matches!(
        err,
        KernelError::Tool(ToolError::Execution(message)) if message == "hard failure"
    ));
}
