//! `ChannelReactTool`: tool the LLM uses to post a reaction (emoji)
//! to a message via a `ChannelSink`.
//!
//! v1 auto-targets the inbound message of the current run via
//! `Subject::inbound_message_ref()`. Adapter must implement
//! `Channel::react`; channels without reaction support return
//! `ChannelError::Unsupported` and the tool surfaces that as a
//! `ToolError::Execution`.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::action::Action;
use crabgent_core::error::ToolError;
use crabgent_core::owner::Owner;
use crabgent_core::policy::{PolicyDecision, PolicyHook};
use crabgent_core::tool::{Tool, ToolCtx, parse_args};
use crabgent_core::types::ToolResult;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::envelope::MessageRef;
use crate::sink::ChannelSink;
use crate::subject::ChannelSubjectExt;

use super::{channel_error_to_tool_error, soft_result};

const TOOL_NAME: &str = "channel_react";

/// Tool the LLM calls to react to the user's inbound message with an
/// emoji.
///
/// Schema: `{ "emoji": "🎉" }`. The reaction is posted on the inbound
/// message tracked by the kernel for the current run.
pub struct ChannelReactTool {
    sink: Arc<dyn ChannelSink>,
    policy: Arc<dyn PolicyHook>,
}

impl ChannelReactTool {
    /// Build a tool wrapping the given sink.
    #[must_use]
    pub fn new(sink: Arc<dyn ChannelSink>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { sink, policy }
    }
}

#[derive(Debug, Deserialize)]
struct ReactArgs {
    emoji: String,
}

fn render_message_ref(r: &MessageRef) -> Value {
    json!({
        "channel": r.channel,
        "conv": r.conv.as_str(),
        "id": r.id,
        "thread_root": r.thread_root,
        "broadcast": r.broadcast,
    })
}

#[async_trait]
impl Tool for ChannelReactTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Post a reaction (emoji) to the user's inbound message via a \
         channel adapter. Args: `emoji` (the reaction emoji or shortcode). \
         Use ONLY when (a) the user explicitly asked you to react with a \
         specific emoji, or (b) the user's message was a pure \
         acknowledgement signal that needs no informational reply (e.g. \
         a quick 'done' or 'noted'). A reaction is NEVER a substitute for \
         `channel_send` when the user asked a question, asked you to do \
         something that yields information, or expects a textual answer. \
         Channels that do not support reactions return an Unsupported error."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["emoji"],
            "properties": {
                "emoji": {
                    "type": "string",
                    "description": "Reaction emoji or shortcode, e.g. 🎉, 👍, thumbsup."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let parsed: ReactArgs = parse_args(args)?;
        let parent = ctx.subject.inbound_message_ref().ok_or_else(|| {
            ToolError::Execution("no inbound message ref on subject; cannot react".into())
        })?;
        let conv = Owner::new(parent.conv.as_str());
        self.gate(ctx).await?;
        let result = self
            .sink
            .react(&ctx.subject, &conv, &parent, &parsed.emoji)
            .await
            .map_err(channel_error_to_tool_error)?;
        Ok(render_message_ref(&result))
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        soft_result(self.execute(args, ctx).await)
    }
}

impl ChannelReactTool {
    async fn gate(&self, ctx: &ToolCtx) -> Result<(), ToolError> {
        let action = Action::tool(TOOL_NAME);
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
    use crate::sink::ChannelRouter;
    use crate::subject::ChannelSubjectExt;
    use crate::test_support::RecordingChannel;
    use crabgent_core::policy::{AllowAllPolicy, DenyAllPolicy};
    use crabgent_core::subject::Subject;

    fn build_tool() -> (ChannelReactTool, Arc<RecordingChannel>) {
        let stub = Arc::new(RecordingChannel::new("stub", ChannelKind::Group, "ts:42"));
        let trait_obj: Arc<dyn Channel> = Arc::clone(&stub) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        (
            ChannelReactTool::new(router, Arc::new(AllowAllPolicy)),
            stub,
        )
    }

    fn subject_with_inbound() -> Subject {
        let conv = Owner::new("stub:c");
        let parent = MessageRef::top_level("stub", conv.clone(), "ts:user-msg");
        Subject::new("agent")
            .with_channel("stub", &conv, ChannelKind::Group)
            .with_inbound_message_ref(&parent)
    }

    #[tokio::test]
    async fn react_tool_posts_reaction_to_inbound() {
        let (tool, stub) = build_tool();
        let ctx = ToolCtx::new(subject_with_inbound());
        let args = json!({"emoji": "🎉"});
        let r = tool.execute(args, &ctx).await.expect("ok");
        assert_eq!(r["channel"], "stub");
        let (parent, emoji) = stub.last_reaction().expect("recorded reaction");
        assert_eq!(emoji, "🎉");
        assert_eq!(parent.id, "ts:user-msg");
    }

    #[tokio::test]
    async fn react_tool_without_inbound_ref_errors() {
        let (tool, _) = build_tool();
        let ctx = ToolCtx::new(Subject::new("agent"));
        let args = json!({"emoji": "🎉"});
        let err = tool.execute(args, &ctx).await.expect_err("should fail");
        assert!(matches!(err, ToolError::Execution(msg) if msg.contains("inbound")));
    }

    #[tokio::test]
    async fn react_tool_denied_by_policy() {
        let stub = Arc::new(RecordingChannel::new("stub", ChannelKind::Group, "ts:42"));
        let trait_obj: Arc<dyn Channel> = Arc::clone(&stub) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        let tool = ChannelReactTool::new(router, Arc::new(DenyAllPolicy));
        let ctx = ToolCtx::new(subject_with_inbound());
        let args = json!({"emoji": "👍"});
        let err = tool.execute(args, &ctx).await.expect_err("policy deny");
        assert!(matches!(err, ToolError::Permission(_)));
        assert_eq!(stub.react_count(), 0);
    }

    #[tokio::test]
    async fn react_tool_execute_result_wraps_policy_deny_as_soft_error() {
        let stub = Arc::new(RecordingChannel::new("stub", ChannelKind::Group, "ts:42"));
        let trait_obj: Arc<dyn Channel> = Arc::clone(&stub) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        let tool = ChannelReactTool::new(router, Arc::new(DenyAllPolicy));
        let ctx = ToolCtx::new(subject_with_inbound());
        let args = json!({"emoji": "👍"});
        let result = tool.execute_result(args, &ctx).await.expect("soft wrap");
        assert!(result.is_error, "deny must produce soft error, not success");
        assert_eq!(stub.react_count(), 0);
    }

    #[test]
    fn react_tool_metadata_sane() {
        let (tool, _) = build_tool();
        assert_eq!(tool.name(), TOOL_NAME);
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "emoji");
    }
}
