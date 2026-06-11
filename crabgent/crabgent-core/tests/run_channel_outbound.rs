//! Integration tests for the `channel_outbound` message-log append path.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, LlmRequest, LlmResponse, Message, ModelInfo, Owner,
    Provider, ProviderCapabilities, ProviderError, RunCtx, RunId, RunRequest, Subject, Tool,
    ToolCtx, ToolError, ToolResult,
};
use crabgent_test_support::{done, tool_call, tool_use};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[path = "common/noop_tool.rs"]
mod noop_tool;

use noop_tool::NoopTool;

struct RecordingProvider {
    responses: Mutex<Vec<LlmResponse>>,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl RecordingProvider {
    #[expect(
        clippy::missing_const_for_fn,
        reason = "test provider constructor builds Mutex state from runtime inputs"
    )]
    fn with(responses: Vec<LlmResponse>, requests: Arc<Mutex<Vec<LlmRequest>>>) -> Self {
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
        vec![ModelInfo::minimal("m", "recording")]
    }
}

struct ChannelSendTool;

#[async_trait]
impl Tool for ChannelSendTool {
    fn name(&self) -> &'static str {
        "channel_send"
    }

    fn description(&self) -> &'static str {
        "send a message to a channel"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(json!({
            "channel": "slack",
            "conv": "slack:T1/C1",
            "id": "1234.5678",
            "thread_root": null,
            "broadcast": false,
        }))
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        let body = args
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let output = self.execute(args, ctx).await?;
        Ok(
            ToolResult::success(output).with_run_message(Message::ChannelOutbound {
                conv: Owner::new("slack:T1/C1"),
                body,
                channel: "slack".into(),
                message_id: "1234.5678".into(),
                thread_root: None,
                broadcast: false,
            }),
        )
    }
}

struct FailChannelSendTool;

#[async_trait]
impl Tool for FailChannelSendTool {
    fn name(&self) -> &'static str {
        "channel_send"
    }

    fn description(&self) -> &'static str {
        "fails"
    }

    fn parameters_schema(&self) -> Value {
        json!({})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Err(ToolError::Execution("channel error".into()))
    }
}

fn calling_tool(name: &str, id: &str, args: Value) -> LlmResponse {
    tool_use(vec![tool_call(id, name, args)])
}

fn run_req(msg: &str) -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("test-user"),
        model: "m".into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: msg.to_owned(),
            }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(10),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

#[tokio::test]
async fn test_channel_send_appends_outbound_after_tool_result() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(RecordingProvider::with(
            vec![
                calling_tool(
                    "channel_send",
                    "call-1",
                    json!({"conv":"slack:T1/C1","body":"hello world","thread_parent":null}),
                ),
                done("done"),
            ],
            Arc::clone(&requests),
        ))
        .policy(AllowAllPolicy)
        .add_tool(ChannelSendTool)
        .build();

    let result = kernel.run(run_req("send a message"), None).await;
    assert!(result.is_ok(), "run should succeed: {:?}", result.err());

    let rec = requests.lock().expect("mutex should not be poisoned");
    let msgs = &rec[1].messages;
    // Order: [User, Assistant, ToolResult, ChannelOutbound]
    let roles: Vec<&str> = msgs
        .iter()
        .filter_map(|m| m.get("role").and_then(|r| r.as_str()))
        .collect();
    assert!(
        roles.len() >= 4,
        "expected at least 4 messages, got {roles:?}"
    );
    assert_eq!(roles[0], "user");
    assert_eq!(roles[1], "assistant");
    assert_eq!(roles[2], "tool_result");
    assert_eq!(roles[3], "channel_outbound");
}

#[tokio::test]
async fn test_channel_send_error_no_outbound() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(RecordingProvider::with(
            vec![calling_tool(
                "channel_send",
                "call-1",
                json!({"body":"fail"}),
            )],
            Arc::clone(&requests),
        ))
        .policy(AllowAllPolicy)
        .add_tool(FailChannelSendTool)
        .build();

    let _ignored = kernel.run(run_req("send a message"), None).await;
    let rec = requests.lock().expect("mutex should not be poisoned");
    let msgs = &rec[0].messages;
    let has_outbound = msgs
        .iter()
        .any(|m| m.get("role").and_then(Value::as_str) == Some("channel_outbound"));
    assert!(!has_outbound, "should not have channel_outbound on error");
}

#[tokio::test]
async fn test_other_tools_no_outbound() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(RecordingProvider::with(
            vec![calling_tool("noop", "call-1", json!({})), done("done")],
            Arc::clone(&requests),
        ))
        .policy(AllowAllPolicy)
        .add_tool(NoopTool)
        .build();

    let result = kernel.run(run_req("run noop"), None).await;
    result.expect("test result");

    let rec = requests.lock().expect("mutex should not be poisoned");
    let msgs = &rec[1].messages;
    let has_outbound = msgs
        .iter()
        .any(|m| m.get("role").and_then(Value::as_str) == Some("channel_outbound"));
    assert!(
        !has_outbound,
        "non-channel_send tool should not produce channel_outbound"
    );
}
