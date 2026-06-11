//! `ChannelSendTool`: tool the LLM uses to send a message via a
//! `ChannelSink`.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::owner::Owner;
use crabgent_core::policy::{PolicyDecision, PolicyHook};
use crabgent_core::tool::{Tool, ToolCtx, parse_args};
use crabgent_core::types::ToolResult;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::action::{channel_name_from_owner, channel_send_action};
use crate::envelope::{MessageRef, OutboundMessage};
use crate::sink::ChannelSink;
use crate::subject::ChannelSubjectExt;

use super::{
    MessageRefLocation, channel_error_to_tool_error, execute_result_with_outbound,
    render_message_ref,
};

/// Tool the LLM calls to send a message via a `ChannelSink`.
///
/// Schema:
/// ```json
/// {
///   "conv": "slack:T1/C1",
///   "body": "hi",
///   "thread_parent": {
///     "channel": "slack",
///     "conv": "slack:T1/C1",
///     "id": "ts:1700000000.000100",
///     "thread_root": null,
///     "broadcast": false
///   }
/// }
/// ```
/// `thread_parent` is optional. When absent the tool defaults to
/// `ctx.subject.inbound_message_ref()` so a reply lands in the thread
/// of the message that triggered the run. If the subject has no
/// inbound message ref (e.g. for a cron-triggered or
/// otherwise-eigeninitiated run) the message is top-level.
///
/// `top_level: true` short-circuits the inbound-ref default and posts
/// the reply at the conversation root even when an inbound ref is
/// available. Use this for adapters where mid-conversation threading
/// is visually broken (Matrix DMs collapse threads to invisible
/// nesting; Telegram has no threads at all). Mutually exclusive with
/// `thread_parent`: a request that sets both is rejected with
/// `InvalidArgs`.
pub struct ChannelSendTool {
    sink: Arc<dyn ChannelSink>,
    policy: Arc<dyn PolicyHook>,
}

impl ChannelSendTool {
    /// Build a tool wrapping the given sink.
    #[must_use]
    pub fn new(sink: Arc<dyn ChannelSink>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { sink, policy }
    }
}

#[derive(Debug, Deserialize)]
struct SendArgs {
    conv: String,
    body: String,
    #[serde(default)]
    thread_parent: Option<MessageRefArgs>,
    #[serde(default)]
    top_level: bool,
}

#[derive(Debug, Deserialize)]
struct MessageRefArgs {
    channel: String,
    conv: String,
    id: String,
    #[serde(default)]
    thread_root: Option<String>,
    #[serde(default)]
    broadcast: bool,
}

impl From<MessageRefArgs> for MessageRef {
    fn from(args: MessageRefArgs) -> Self {
        Self {
            channel: args.channel,
            conv: Owner::new(args.conv),
            id: args.id,
            thread_root: args.thread_root,
            broadcast: args.broadcast,
        }
    }
}

#[async_trait]
impl Tool for ChannelSendTool {
    fn name(&self) -> &'static str {
        "channel_send"
    }
    fn description(&self) -> &'static str {
        "Send a message into a conversation via a channel adapter. \
         Specify `conv` (the conversation Owner string) and `body`. \
         Replies default to the thread of the inbound message; pass \
         `thread_parent` to target a different thread or omit it for \
         the inbound thread. Pass `top_level: true` to force a root-of- \
         conversation reply even when an inbound ref is in scope (e.g. \
         for adapters where mid-conversation threading is broken)."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["conv", "body"],
            "properties": {
                "conv": {
                    "type": "string",
                    "description": "Conversation owner string, e.g. \"slack:T1/C1\"."
                },
                "body": {
                    "type": "string",
                    "description": "Plain text or basic Markdown body. The channel adapter normalizes it to the target wire format."
                },
                "thread_parent": {
                    "type": "object",
                    "description": "Optional thread anchor. When omitted the tool defaults to the inbound message ref of the current run, so replies thread automatically. Pass an explicit ref to target a different thread.",
                    "required": ["channel", "conv", "id"],
                    "properties": {
                        "channel": { "type": "string" },
                        "conv": { "type": "string" },
                        "id": { "type": "string" },
                        "thread_root": { "type": ["string", "null"] },
                        "broadcast": {
                            "type": "boolean",
                            "default": false,
                            "description": "Broadcast a thread reply to the parent timeline when the adapter supports it."
                        }
                    }
                },
                "top_level": {
                    "type": "boolean",
                    "default": false,
                    "description": "Force a root-of-conversation reply even when an inbound ref is in scope. Skips the inbound-ref default that would otherwise thread the reply. Mutually exclusive with `thread_parent`."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let parsed: SendArgs = parse_args(args)?;
        if parsed.top_level && parsed.thread_parent.is_some() {
            return Err(ToolError::InvalidArgs(
                "top_level=true and thread_parent are mutually exclusive".into(),
            ));
        }
        let conv = Owner::new(parsed.conv);
        let Some(channel) = channel_name_from_owner(&conv) else {
            return Err(ToolError::InvalidArgs(
                "conv must be '<channel>:<rest>' format".into(),
            ));
        };
        let mut msg = OutboundMessage::new(parsed.body);
        if let Some(parent) = parsed.thread_parent {
            msg = msg.in_thread(parent.into());
        } else if !parsed.top_level
            && let Some(inbound) = ctx.subject.inbound_message_ref()
        {
            msg = msg.in_thread(inbound);
        }
        self.gate(ctx, Some(channel), &conv).await?;
        let result = self
            .sink
            .send(&ctx.subject, &conv, &msg)
            .await
            .map_err(channel_error_to_tool_error)?;
        Ok(render_message_ref(&result))
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        execute_result_with_outbound(self, args, ctx, "body", MessageRefLocation::Output).await
    }
}

impl ChannelSendTool {
    async fn gate(
        &self,
        ctx: &ToolCtx,
        channel: Option<&str>,
        conv: &Owner,
    ) -> Result<(), ToolError> {
        let action = channel_send_action(channel, conv);
        match self.policy.allow(&ctx.subject, &action).await {
            PolicyDecision::Allow => Ok(()),
            PolicyDecision::Deny(reason) => Err(ToolError::Permission(reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{Channel, ChannelKind};
    use crate::error::ChannelError;
    use crate::sink::ChannelRouter;
    use crate::test_support::RecordingChannel;
    use crabgent_core::policy::{AllowAllPolicy, DenyAllPolicy};
    use crabgent_core::subject::Subject;

    struct PanicSink;

    #[async_trait]
    impl ChannelSink for PanicSink {
        async fn send(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
            _msg: &OutboundMessage,
        ) -> Result<MessageRef, ChannelError> {
            panic!("sink should not be called")
        }

        async fn react(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
            _parent: &MessageRef,
            _emoji: &str,
        ) -> Result<MessageRef, ChannelError> {
            panic!("sink should not be called")
        }
    }

    struct CancelSink;

    #[async_trait]
    impl ChannelSink for CancelSink {
        async fn send(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
            _msg: &OutboundMessage,
        ) -> Result<MessageRef, ChannelError> {
            Err(ChannelError::Cancelled)
        }

        async fn react(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
            _parent: &MessageRef,
            _emoji: &str,
        ) -> Result<MessageRef, ChannelError> {
            panic!("sink should not be called")
        }
    }

    fn build_tool() -> (ChannelSendTool, Arc<RecordingChannel>) {
        let stub = Arc::new(RecordingChannel::new("stub", ChannelKind::Group, "ts:42"));
        let trait_obj: Arc<dyn Channel> = Arc::clone(&stub) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        (ChannelSendTool::new(router, Arc::new(AllowAllPolicy)), stub)
    }

    #[tokio::test]
    async fn send_tool_dispatches_top_level() {
        let (tool, stub) = build_tool();
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({"conv": "stub:c", "body": "hi"});
        let r = tool.execute(args, &ctx).await.expect("ok");
        assert_eq!(r["channel"], "stub");
        assert_eq!(r["id"], "ts:42");
        assert!(r["thread_root"].is_null());
        assert_eq!(r["broadcast"], false);
        let recorded = stub.last_sent().expect("recorded send");
        assert_eq!(recorded.body, "hi");
        assert!(recorded.thread_parent.is_none());
    }

    #[tokio::test]
    async fn send_tool_execute_result_records_channel_outbound() {
        let (tool, _) = build_tool();
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({"conv": "stub:c", "body": "hi"});

        let result = tool.execute_result(args, &ctx).await.expect("ok");

        super::super::assert_single_outbound(&result, "stub:c", "hi", "stub", "ts:42");
    }

    #[tokio::test]
    async fn send_tool_defaults_thread_parent_to_inbound_message_ref() {
        let (tool, stub) = build_tool();
        let inbound = MessageRef::top_level("stub", Owner::new("stub:c"), "ts:inbound");
        let subject = Subject::new("agent")
            .with_channel("stub", &Owner::new("stub:c"), ChannelKind::Group)
            .with_inbound_message_ref(&inbound);
        let ctx = ToolCtx::new(subject);
        let args = json!({"conv": "stub:c", "body": "hi"});
        tool.execute(args, &ctx).await.expect("ok");
        let recorded = stub.last_sent().expect("recorded send");
        let parent = recorded
            .thread_parent
            .as_ref()
            .expect("default thread parent");
        assert_eq!(parent.id, "ts:inbound");
    }

    #[tokio::test]
    async fn send_tool_top_level_flag_skips_inbound_thread_default() {
        let (tool, stub) = build_tool();
        let inbound = MessageRef::top_level("stub", Owner::new("stub:c"), "ts:inbound");
        let subject = Subject::new("agent")
            .with_channel("stub", &Owner::new("stub:c"), ChannelKind::Group)
            .with_inbound_message_ref(&inbound);
        let ctx = ToolCtx::new(subject);
        let args = json!({"conv": "stub:c", "body": "hi", "top_level": true});
        tool.execute(args, &ctx).await.expect("ok");
        let recorded = stub.last_sent().expect("recorded send");
        assert!(
            recorded.thread_parent.is_none(),
            "top_level=true must not attach inbound as thread_parent"
        );
    }

    #[tokio::test]
    async fn send_tool_top_level_with_thread_parent_is_invalid() {
        let (tool, _) = build_tool();
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({
            "conv": "stub:c",
            "body": "hi",
            "top_level": true,
            "thread_parent": {
                "channel": "stub",
                "conv": "stub:c",
                "id": "ts:1",
            }
        });
        let err = tool
            .execute(args, &ctx)
            .await
            .expect_err("mutually exclusive");
        assert!(matches!(
            err,
            ToolError::InvalidArgs(msg) if msg.contains("mutually exclusive")
        ));
    }

    #[tokio::test]
    async fn send_tool_dispatches_thread_reply() {
        let (tool, stub) = build_tool();
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({
            "conv": "stub:c",
            "body": "reply",
            "thread_parent": {
                "channel": "stub",
                "conv": "stub:c",
                "id": "ts:1",
                "thread_root": null,
                "broadcast": true
            }
        });
        let _ = tool.execute(args, &ctx).await.expect("ok");
        let recorded = stub.last_sent().expect("recorded send");
        let parent = recorded.thread_parent.as_ref().expect("thread parent");
        assert!(parent.broadcast());
    }

    #[tokio::test]
    async fn send_tool_invalid_args_returns_error() {
        let (tool, _) = build_tool();
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({"conv": 42}); // wrong type, missing body
        let err = tool.execute(args, &ctx).await.expect_err("should fail");
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn send_tool_invalid_conv_returns_invalid_args_before_sink() {
        let tool = ChannelSendTool::new(Arc::new(PanicSink), Arc::new(AllowAllPolicy));
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({"conv": "noprefix", "body": "hi"});
        let err = tool
            .execute(args, &ctx)
            .await
            .expect_err("invalid conv should fail");

        assert!(
            matches!(err, ToolError::InvalidArgs(message) if message.contains("<channel>:<rest>"))
        );
    }

    #[tokio::test]
    async fn send_tool_unregistered_channel_maps_to_not_found() {
        let stub: Arc<dyn Channel> =
            Arc::new(RecordingChannel::new("stub", ChannelKind::Group, "ts:42"));
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(stub));
        let tool = ChannelSendTool::new(router, Arc::new(AllowAllPolicy));
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({"conv": "telegram:42", "body": "hi"});
        let err = tool.execute(args, &ctx).await.expect_err("fail");
        match err {
            ToolError::NotFound(name) => assert_eq!(name, "telegram"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_tool_denies_before_channel_send() {
        let stub = Arc::new(RecordingChannel::new("stub", ChannelKind::Group, "ts:42"));
        let trait_obj: Arc<dyn Channel> = Arc::clone(&stub) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        let tool = ChannelSendTool::new(router, Arc::new(DenyAllPolicy));
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({"conv": "stub:c", "body": "hi"});
        let err = tool.execute(args, &ctx).await.expect_err("policy deny");
        assert!(matches!(err, ToolError::Permission(_)));
        assert_eq!(stub.sent_count(), 0);
    }

    #[tokio::test]
    async fn send_tool_execute_result_wraps_policy_deny_as_soft_error() {
        let stub = Arc::new(RecordingChannel::new("stub", ChannelKind::Group, "ts:42"));
        let trait_obj: Arc<dyn Channel> = Arc::clone(&stub) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        let tool = ChannelSendTool::new(router, Arc::new(DenyAllPolicy));
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({"conv": "stub:c", "body": "hi"});
        let result = tool.execute_result(args, &ctx).await.expect("soft wrap");
        assert!(result.is_error, "deny must produce soft error, not success");
        assert_eq!(stub.sent_count(), 0);
    }

    #[tokio::test]
    async fn send_tool_execute_result_propagates_cancelled_as_hard_error() {
        let tool = ChannelSendTool::new(Arc::new(CancelSink), Arc::new(AllowAllPolicy));
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({"conv": "stub:c", "body": "hi"});

        let err = tool
            .execute_result(args, &ctx)
            .await
            .expect_err("cancelled send should stay hard");

        assert!(matches!(err, ToolError::Cancelled));
    }

    #[test]
    fn send_tool_metadata_sane() {
        let (tool, _) = build_tool();
        assert_eq!(tool.name(), "channel_send");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "conv");
        assert_eq!(schema["required"][1], "body");
    }
}
