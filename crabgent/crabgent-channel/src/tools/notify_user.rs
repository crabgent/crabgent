//! `NotifyUserTool`: tool the LLM uses to message a user out-of-band
//! via `ChannelSink::notify_user`.
//!
//! Unlike `channel_send`, this tool addresses a user by channel-specific
//! id and does not need an existing conversation: the adapter opens or
//! reuses a direct conversation with the recipient. The delivered
//! message is recorded as a `Message::ChannelOutbound` owned by this tool,
//! so it persists into the recipient DM session.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::action::Action;
use crabgent_core::error::ToolError;
use crabgent_core::policy::{PolicyDecision, PolicyHook};
use crabgent_core::tool::{Tool, ToolCtx, parse_args};
use crabgent_core::types::ToolResult;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::envelope::OutboundMessage;
use crate::participant::ParticipantId;
use crate::sink::ChannelSink;

use super::{
    MessageRefLocation, channel_error_to_tool_error, execute_result_with_outbound,
    render_message_ref,
};

const TOOL_NAME: &str = "notify_user";

/// Tool the LLM calls to notify a user out-of-band.
///
/// Schema: `{ "channel": "slack", "participant_id": "U123", "body": "..." }`.
/// The adapter opens or reuses a direct conversation with the recipient.
pub struct NotifyUserTool {
    sink: Arc<dyn ChannelSink>,
    policy: Arc<dyn PolicyHook>,
}

impl NotifyUserTool {
    /// Build a tool wrapping the given sink.
    #[must_use]
    pub fn new(sink: Arc<dyn ChannelSink>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { sink, policy }
    }
}

#[derive(Debug, Deserialize)]
struct NotifyArgs {
    channel: String,
    participant_id: String,
    body: String,
}

#[async_trait]
impl Tool for NotifyUserTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Notify a known user out-of-band by opening or reusing a direct \
         conversation with them. Args: `channel` (adapter name, e.g. \
         \"slack\", \"matrix\", \"telegram\"), `participant_id` (the \
         channel-specific user id of the recipient), and `body` (plain \
         text or basic Markdown; the adapter normalizes it to the target \
         wire format). Use this to reach a user you have no \
         existing conversation with; use `channel_send` to reply inside \
         an existing conversation. Telegram requires the user to have \
         messaged the bot at least once, otherwise delivery fails."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["channel", "participant_id", "body"],
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Adapter name: slack, matrix, or telegram."
                },
                "participant_id": {
                    "type": "string",
                    "description": "Channel-specific recipient id (Slack U..., Matrix @user:server, Telegram numeric id)."
                },
                "body": {
                    "type": "string",
                    "description": "Plain text or basic Markdown body. The channel adapter normalizes it to the target wire format."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let parsed: NotifyArgs = parse_args(args)?;
        self.gate(ctx).await?;
        let recipient = ParticipantId::new(parsed.participant_id);
        let msg = OutboundMessage::new(parsed.body).with_metadata("channel", parsed.channel);
        let result = self
            .sink
            .notify_user(&ctx.subject, &recipient, &msg)
            .await
            .map_err(channel_error_to_tool_error)?;
        Ok(render_message_ref(&result))
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        execute_result_with_outbound(self, args, ctx, "body", MessageRefLocation::Output).await
    }
}

impl NotifyUserTool {
    async fn gate(&self, ctx: &ToolCtx) -> Result<(), ToolError> {
        match self
            .policy
            .allow(&ctx.subject, &Action::tool(TOOL_NAME))
            .await
        {
            PolicyDecision::Allow => Ok(()),
            PolicyDecision::Deny(reason) => Err(ToolError::Permission(reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{Channel, ChannelKind};
    use crate::sink::ChannelRouter;
    use crate::test_support::RecordingChannel;
    use crabgent_core::policy::{AllowAllPolicy, DenyAllPolicy};
    use crabgent_core::subject::Subject;

    fn build_tool() -> (NotifyUserTool, Arc<RecordingChannel>) {
        let stub = Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "ts:99"));
        let trait_obj: Arc<dyn Channel> = Arc::clone(&stub) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        (NotifyUserTool::new(router, Arc::new(AllowAllPolicy)), stub)
    }

    #[tokio::test]
    async fn notify_tool_dispatches_via_metadata_channel() {
        let (tool, stub) = build_tool();
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({
            "channel": "slack",
            "participant_id": "U-alice",
            "body": "your report is ready"
        });
        let r = tool.execute(args, &ctx).await.expect("ok");
        assert_eq!(r["channel"], "slack");
        assert_eq!(r["id"], "ts:99");
        assert_eq!(stub.notify_user_count(), 1);
    }

    #[tokio::test]
    async fn notify_tool_execute_result_records_channel_outbound() {
        let (tool, _) = build_tool();
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({
            "channel": "slack",
            "participant_id": "U-alice",
            "body": "your report is ready"
        });

        let result = tool.execute_result(args, &ctx).await.expect("ok");

        super::super::assert_single_outbound(
            &result,
            "slack:notify/U-alice",
            "your report is ready",
            "slack",
            "ts:99",
        );
    }

    #[tokio::test]
    async fn notify_tool_invalid_args_returns_error() {
        let (tool, _) = build_tool();
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({"channel": "slack"}); // missing participant_id + body
        let err = tool.execute(args, &ctx).await.expect_err("should fail");
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn notify_tool_unregistered_channel_maps_to_not_found() {
        let stub: Arc<dyn Channel> =
            Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "ts:99"));
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(stub));
        let tool = NotifyUserTool::new(router, Arc::new(AllowAllPolicy));
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({
            "channel": "telegram",
            "participant_id": "42",
            "body": "hi"
        });
        let err = tool.execute(args, &ctx).await.expect_err("fail");
        match err {
            ToolError::NotFound(name) => assert_eq!(name, "telegram"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn notify_tool_empty_channel_maps_to_invalid_args() {
        let (tool, _) = build_tool();
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({
            "channel": "",
            "participant_id": "U-alice",
            "body": "hi"
        });
        let err = tool
            .execute(args, &ctx)
            .await
            .expect_err("empty channel should fail");
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn notify_tool_denies_before_sink() {
        let stub = Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "ts:99"));
        let trait_obj: Arc<dyn Channel> = Arc::clone(&stub) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        let tool = NotifyUserTool::new(router, Arc::new(DenyAllPolicy));
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({
            "channel": "slack",
            "participant_id": "U-alice",
            "body": "hi"
        });
        let err = tool.execute(args, &ctx).await.expect_err("policy deny");
        assert!(matches!(err, ToolError::Permission(_)));
        assert_eq!(stub.notify_user_count(), 0);
    }

    #[tokio::test]
    async fn notify_tool_execute_result_wraps_policy_deny_as_soft_error() {
        let stub = Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "ts:99"));
        let trait_obj: Arc<dyn Channel> = Arc::clone(&stub) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        let tool = NotifyUserTool::new(router, Arc::new(DenyAllPolicy));
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({
            "channel": "slack",
            "participant_id": "U-alice",
            "body": "hi"
        });
        let result = tool.execute_result(args, &ctx).await.expect("soft wrap");
        assert!(result.is_error, "deny must produce soft error, not success");
        assert_eq!(stub.notify_user_count(), 0);
    }

    #[test]
    fn notify_tool_metadata_sane() {
        let (tool, _) = build_tool();
        assert_eq!(tool.name(), "notify_user");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "channel");
        assert_eq!(schema["required"][1], "participant_id");
        assert_eq!(schema["required"][2], "body");
    }
}
