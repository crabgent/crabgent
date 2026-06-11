//! Regression: an `update_file` call against a missing path must not
//! abort the kernel run. `execute_result` at `update_file.rs:109` converts
//! `ToolError::NotFound` (path does not exist) into a soft error so the
//! LLM can repair the call: pick a correct path, create the file first,
//! or stop. Before the fix the run aborted with `Outcome::Errored` and
//! consumer-side `on_stop` hooks never saw the tool output.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, LlmRequest, LlmResponse, Message, ModelInfo, Provider,
    ProviderCapabilities, ProviderError, RunCtx, RunId, RunRequest, Subject, UpdateFileTool,
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
                text: "edit the config file".into(),
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
async fn update_file_missing_path_soft_errors_and_run_completes() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(RecordingProvider::with(
            vec![
                calling_tool(
                    "update_file",
                    "call-1",
                    json!({
                        "path": "/no/such/path/here.txt",
                        "old_str": "sentinel",
                        "new_str": "x"
                    }),
                ),
                done("could not find the file to edit"),
            ],
            requests.clone(),
        ))
        .policy(AllowAllPolicy)
        .add_tool(UpdateFileTool::without_root())
        .build();

    let text = kernel
        .run(make_request(), None)
        .await
        .expect("run should complete, not error");
    assert_eq!(text, "could not find the file to edit");

    let recorded = requests
        .lock()
        .expect("requests mutex must not be poisoned");
    assert_eq!(
        recorded.len(),
        2,
        "missing-path update_file must feed back through the LLM, not abort the run"
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
        .expect("output field must be a string in the raw message");
    assert_eq!(
        output,
        "<tool_error tool=\"update_file\">{\"error\":\"path not found\"}</tool_error>"
    );
    assert!(!output.contains("/no/such/path/here.txt"));
}

#[tokio::test]
async fn anchor_not_found_soft_errors_via_execute_result() {
    let dir = tempdir().expect("tempdir must be created");
    let path = dir.path().join("target.txt");
    std::fs::write(&path, "hello world").expect("test file must be written");

    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(RecordingProvider::with(
            vec![
                calling_tool(
                    "update_file",
                    "call-1",
                    json!({
                        "path": path.to_str().expect("path should be valid UTF-8"),
                        "old_str": "zzz_not_present",
                        "new_str": "replacement"
                    }),
                ),
                done("could not find the anchor to edit"),
            ],
            requests.clone(),
        ))
        .policy(AllowAllPolicy)
        .add_tool(UpdateFileTool::without_root())
        .build();

    let text = kernel
        .run(make_request(), None)
        .await
        .expect("run should complete, not error");
    assert_eq!(text, "could not find the anchor to edit");
    assert_eq!(
        std::fs::read_to_string(&path).expect("test file must remain readable"),
        "hello world"
    );

    let recorded = requests
        .lock()
        .expect("requests mutex must not be poisoned");
    assert_eq!(
        recorded.len(),
        2,
        "missing-anchor update_file must feed back through the LLM, not abort the run"
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
        .expect("output field must be a string in the raw message");
    assert!(
        output.contains("anchor not found"),
        "soft-error payload should surface the missing anchor so the LLM can repair: got {output}"
    );
}
