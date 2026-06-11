//! Regression: a `read_file` call against a missing path must not
//! abort the kernel run. The recoverable `ToolError::NotFound` should
//! land in the LLM message history as `ToolResult { is_error: true }`
//! so the model can repair the call. Before the fix the run aborted
//! with `Outcome::Errored` and consumer-side `on_stop` hooks never saw
//! the tool output.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, LlmRequest, LlmResponse, Message, ModelInfo, Provider,
    ProviderCapabilities, ProviderError, ReadFileTool, RunCtx, RunId, RunRequest, Subject,
};
use crabgent_test_support::{done, tool_call, tool_use};

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
            .expect("requests mutex must not be poisoned")
            .push(req.clone());
        let mut queue = self
            .responses
            .lock()
            .expect("responses mutex must not be poisoned");
        if queue.is_empty() {
            return Err(ProviderError::Other("script exhausted".into()));
        }
        Ok(queue.remove(0))
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
                text: "open the memory file".into(),
            }],
            timestamp: None,
        }],
        system_prompt: None,
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
        .expect("tool result message present in next provider request")
}

#[tokio::test]
async fn read_file_missing_path_soft_errors_and_run_completes() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(RecordingProvider::with(
            vec![
                calling_tool(
                    "read_file",
                    "call-1",
                    json!({"path": "/no/such/path/here.txt"}),
                ),
                done("could not find the memory file"),
            ],
            requests.clone(),
        ))
        .policy(AllowAllPolicy)
        .add_tool(ReadFileTool::without_root())
        .build();

    let text = kernel
        .run(make_request(), None)
        .await
        .expect("run should complete, not error");
    assert_eq!(text, "could not find the memory file");

    let recorded = requests
        .lock()
        .expect("requests mutex must not be poisoned");
    assert_eq!(
        recorded.len(),
        2,
        "missing-path read_file must feed back through the LLM, not abort the run"
    );
    let result = recorded_tool_result(
        recorded
            .get(1)
            .expect("second provider request must follow the tool call"),
        "call-1",
    );
    assert_eq!(
        result.get("is_error"),
        Some(&json!(true)),
        "tool result must carry is_error=true so the LLM can react"
    );
    let output = result
        .get("output")
        .and_then(Value::as_str)
        .expect("output");
    assert_eq!(
        output,
        "<tool_error tool=\"read_file\">{\"error\":\"path not found\"}</tool_error>"
    );
    assert!(!output.contains("/no/such/path/here.txt"));
}
