mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

use common::{NoopTool, RecordingProvider, calling_tool, done};
use crabgent_core::{
    Action, ContentBlock, Kernel, KernelError, LlmRequest, MemoryScope, Message, PolicyDecision,
    PolicyHook, RunId, RunRequest, Subject, Tool, ToolCtx, ToolError, ToolResult,
};

struct MemorySearchSimulatorTool {
    policy: Arc<dyn PolicyHook>,
}

#[async_trait]
impl Tool for MemorySearchSimulatorTool {
    fn name(&self) -> &'static str {
        "memory_sim"
    }

    fn description(&self) -> &'static str {
        "test tool that gates internally on Action::MemorySearch"
    }

    fn parameters_schema(&self) -> Value {
        json!({})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Err(ToolError::Execution(
            "MemorySearchSimulatorTool uses execute_result".into(),
        ))
    }

    async fn execute_result(&self, _args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        let action = Action::MemorySearch {
            query: "test".into(),
            scope: MemoryScope::global(),
        };
        match self.policy.allow(&ctx.subject, &action).await {
            PolicyDecision::Allow => Ok(ToolResult::success(json!({"hits": []}))),
            PolicyDecision::Deny(reason) => Err(ToolError::Permission(reason)),
        }
    }
}

struct DenyMemorySearchPolicy;

#[async_trait]
impl PolicyHook for DenyMemorySearchPolicy {
    async fn allow(&self, _subject: &Subject, action: &Action) -> PolicyDecision {
        match action {
            Action::LlmCall | Action::ToolCall(_) => PolicyDecision::Allow,
            Action::MemorySearch { .. } => {
                PolicyDecision::Deny("memory search not allowed in this scope".into())
            }
            other => PolicyDecision::Deny(format!("unexpected action {}", other.name())),
        }
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
            content: vec![ContentBlock::Text {
                text: "search memory".into(),
            }],
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
async fn inner_policy_deny_surfaces_as_tool_result_and_continues_run() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(DenyMemorySearchPolicy);
    let kernel = Kernel::builder()
        .provider(RecordingProvider::with(
            vec![
                calling_tool("memory_sim", "call-1", json!({})),
                done("found nothing"),
            ],
            requests.clone(),
        ))
        .policy(DenyMemorySearchPolicy)
        .add_tool(NoopTool)
        .add_tool(MemorySearchSimulatorTool { policy })
        .build();

    let text = kernel.run(request(), None).await.expect("run continues");

    assert_eq!(text, "found nothing");
    assert_run_soft_denied_recorded_request(
        &requests.lock().expect("requests mutex not poisoned"),
        "call-1",
        "memory search not allowed in this scope",
    );
}

#[tokio::test]
async fn inner_tool_execution_error_still_aborts_run() {
    let kernel = Kernel::builder()
        .provider(RecordingProvider::with(
            vec![calling_tool("hard_error", "call-1", json!({}))],
            Arc::new(Mutex::new(Vec::new())),
        ))
        .policy(DenyMemorySearchPolicy)
        .add_tool(NoopTool)
        .add_tool(HardErrorTool)
        .build();

    let err = kernel
        .run(request(), None)
        .await
        .expect_err("hard tool error");

    assert!(matches!(
        err,
        KernelError::Tool(ToolError::Execution(message)) if message == "hard failure"
    ));
}
