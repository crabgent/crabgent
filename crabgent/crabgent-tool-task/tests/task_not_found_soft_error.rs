//! Regression: a `task` tool call against a non-existent task id must not
//! abort the kernel run. The recoverable `ToolError::NotFound` must land in
//! the LLM message history as `ToolResult { is_error: true }` so the model
//! can repair the call. Before the core run-loop mapping covered `NotFound`,
//! the run aborted with `Outcome::Errored`.

mod common;

use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, KernelBuilder, LlmRequest, LlmResponse, Message,
    ModelInfo, Provider, ProviderCapabilities, ProviderError, RunCtx, RunId, RunRequest, Subject,
};
use crabgent_store::{MemoryTaskStore, TaskId};
use crabgent_task::TaskExecutor;
use crabgent_test_support::{done, tool_call, tool_use};
use crabgent_tool_task::TaskTool;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

// ------------------------------------------------------------------ helpers

struct ScriptedProvider {
    responses: Mutex<Vec<LlmResponse>>,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl ScriptedProvider {
    const fn new(responses: Vec<LlmResponse>, requests: Arc<Mutex<Vec<LlmRequest>>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            requests,
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.requests
            .lock()
            .expect("requests lock must not be poisoned")
            .push(req.clone());
        let mut queue = self
            .responses
            .lock()
            .expect("responses lock must not be poisoned");
        if queue.is_empty() {
            return Err(ProviderError::Other("script exhausted".into()));
        }
        Ok(queue.remove(0))
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
        vec![ModelInfo::minimal("test", "scripted")]
    }
}

fn calling_tool(name: &str, id: &str, args: Value) -> LlmResponse {
    tool_use(vec![tool_call(id, name, args)])
}

fn make_request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("test-user"),
        model: "test".into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "check the task status".into(),
            }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(5),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
    }
}

fn recorded_tool_result<'a>(req: &'a LlmRequest, call_id: &str) -> &'a Value {
    req.messages
        .iter()
        .find(|msg| {
            msg.get("role").and_then(Value::as_str) == Some("tool_result")
                && msg.get("call_id").and_then(Value::as_str) == Some(call_id)
        })
        .expect("tool result message must be present in next provider request")
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn task_get_unknown_id_soft_errors_and_run_completes() {
    let bad_id = TaskId::new().to_string();
    let requests = Arc::new(Mutex::new(Vec::new()));

    let store = Arc::new(MemoryTaskStore::default());
    let executor = Arc::new(TaskExecutor::new(Arc::clone(&store)));
    let kernel_cell: Arc<OnceLock<Arc<Kernel>>> = Arc::new(OnceLock::new());

    let tool = TaskTool::new_lazy(
        Arc::clone(&executor),
        Arc::clone(&kernel_cell),
        Arc::clone(&store),
        Arc::new(AllowAllPolicy),
    );

    let kernel = Arc::new(
        KernelBuilder::new()
            .provider(ScriptedProvider::new(
                vec![
                    calling_tool("task", "call-1", json!({"op": "get", "task_id": bad_id})),
                    done("task not found, cannot help"),
                ],
                Arc::clone(&requests),
            ))
            .policy(AllowAllPolicy)
            .add_tool(tool)
            .build(),
    );
    drop(kernel_cell.set(Arc::clone(&kernel)));

    let text = kernel
        .run(make_request(), None)
        .await
        .expect("run should complete, not error out on NotFound");
    assert_eq!(text, "task not found, cannot help");

    let recorded = requests.lock().expect("requests lock must not be poisoned");
    assert_eq!(
        recorded.len(),
        2,
        "NotFound from task.get must feed back through the LLM as soft error, not abort the run"
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
        "task.get NotFound must carry is_error=true so the LLM can react"
    );
    let output = result
        .get("output")
        .and_then(Value::as_str)
        .expect("output field must be present");
    assert!(
        output.contains("\"error\""),
        "task.get soft-error payload should keep the legacy JSON error object shape: got {output}"
    );
    assert!(
        output.contains(&bad_id),
        "soft-error payload should surface the missing task id so the LLM can repair: got {output}"
    );
}

#[tokio::test]
async fn task_cancel_unknown_id_soft_errors_and_run_completes() {
    let bad_id = TaskId::new().to_string();
    let requests = Arc::new(Mutex::new(Vec::new()));

    let store = Arc::new(MemoryTaskStore::default());
    let executor = Arc::new(TaskExecutor::new(Arc::clone(&store)));
    let kernel_cell: Arc<OnceLock<Arc<Kernel>>> = Arc::new(OnceLock::new());

    let tool = TaskTool::new_lazy(
        Arc::clone(&executor),
        Arc::clone(&kernel_cell),
        Arc::clone(&store),
        Arc::new(AllowAllPolicy),
    );

    let kernel = Arc::new(
        KernelBuilder::new()
            .provider(ScriptedProvider::new(
                vec![
                    calling_tool("task", "call-2", json!({"op": "cancel", "task_id": bad_id})),
                    done("task not found, nothing to cancel"),
                ],
                Arc::clone(&requests),
            ))
            .policy(AllowAllPolicy)
            .add_tool(tool)
            .build(),
    );
    drop(kernel_cell.set(Arc::clone(&kernel)));

    let text = kernel
        .run(make_request(), None)
        .await
        .expect("run should complete, not error out on NotFound");
    assert_eq!(text, "task not found, nothing to cancel");

    let recorded = requests.lock().expect("requests lock must not be poisoned");
    assert_eq!(
        recorded.len(),
        2,
        "NotFound from task.cancel must feed back through the LLM as soft error, not abort"
    );
    let result = recorded_tool_result(
        recorded
            .get(1)
            .expect("second provider request must follow the tool call"),
        "call-2",
    );
    assert_eq!(
        result.get("is_error"),
        Some(&json!(true)),
        "task.cancel NotFound must carry is_error=true"
    );
    let output = result
        .get("output")
        .and_then(Value::as_str)
        .expect("output field must be present");
    assert!(
        output.contains("\"error\""),
        "task.cancel soft-error payload should keep the legacy JSON error object shape: got {output}"
    );
    assert!(
        output.contains(&bad_id),
        "soft-error payload should surface the missing task id: got {output}"
    );
}
