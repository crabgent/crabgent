use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::{
    ChannelError, ChannelKind, ChannelSink, InboundEvent, MessageRef, OutboundMessage, Participant,
    ParticipantRole,
};
use crabgent_core::{Owner, Subject, Tool, ToolCtx, ToolError};
use crabgent_store::SessionId;
use serde_json::{Value, json};

use super::*;

struct RecordingTool {
    args: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn description(&self) -> &'static str {
        "records args"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        self.args
            .lock()
            .expect("test mutex must not be poisoned")
            .push(args);
        Ok(json!({"ok": true}))
    }
}

struct FailingTool;

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &'static str {
        "failing"
    }

    fn description(&self) -> &'static str {
        "fails"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Err(ToolError::Execution("boom".to_owned()))
    }
}

struct ContextTool;

#[async_trait]
impl Tool for ContextTool {
    fn name(&self) -> &'static str {
        "context"
    }

    fn description(&self) -> &'static str {
        "returns context"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(json!({
            "session_id": ctx.session_id,
            "cancelled": ctx.is_cancelled()
        }))
    }
}

struct NoopSink;

#[async_trait]
impl ChannelSink for NoopSink {
    async fn send(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        _msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        Ok(MessageRef::top_level("test", conv.clone(), "m1"))
    }

    async fn react(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        _parent: &MessageRef,
        _emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        Ok(MessageRef::top_level("test", conv.clone(), "r1"))
    }
}

fn ctx() -> CommandCtx {
    let conv = Owner::new("test:conv");
    let event = InboundEvent {
        channel: "test".to_owned(),
        conv: conv.clone(),
        kind: Some(ChannelKind::Group),
        from: Participant::new("u1", ParticipantRole::Human),
        message: MessageRef::top_level("test", conv, "in1"),
        body: "/tool".to_owned(),
        attachments: Vec::new(),
        timestamp: Utc::now(),
    };
    CommandCtx::new(
        Subject::new("test:u1"),
        SessionId::new(),
        event,
        Arc::new(NoopSink),
    )
}

#[tokio::test]
async fn tool_wrap_passes_args_to_execute() {
    let args = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool {
        args: Arc::clone(&args),
    });
    let wrapper = ToolCommand::new(tool);

    wrapper
        .execute(json!({"name": "alice"}), &ctx())
        .await
        .expect("tool executes");

    let recorded = args.lock().expect("test mutex must not be poisoned");
    assert_eq!(recorded.as_slice(), &[json!({"name": "alice"})]);
}

#[tokio::test]
async fn tool_wrap_stringifies_tool_result_output() {
    let args = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool { args });
    let wrapper = ToolCommand::new(tool);

    let result = wrapper
        .execute(json!({}), &ctx())
        .await
        .expect("tool executes");
    let text = stringify_tool_output(&result.output);

    assert!(text.contains("\"ok\": true"));
}

#[tokio::test]
async fn tool_wrap_propagates_tool_error_as_command_error() {
    let wrapper = ToolCommand::new(Arc::new(FailingTool));
    let err = wrapper
        .execute(json!({}), &ctx())
        .await
        .expect_err("tool error propagates");
    assert!(matches!(err, CommandError::Tool(_)));
}

#[tokio::test]
async fn tool_wrap_propagates_session_id_and_cancel_token() {
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let ctx = ctx().with_cancel(token);
    let session_id = ctx.session_id().to_string();
    let wrapper = ToolCommand::new(Arc::new(ContextTool));

    let result = wrapper
        .execute(json!({}), &ctx)
        .await
        .expect("tool executes");

    assert_eq!(result.output["session_id"], json!(session_id));
    assert_eq!(result.output["cancelled"], json!(true));
}

#[test]
fn stringify_tool_output_returns_string_values_directly() {
    assert_eq!(stringify_tool_output(&json!("plain text")), "plain text");
}
