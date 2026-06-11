//! `ChannelListParticipantsTool`: tool the LLM uses to enumerate
//! participants of a conversation.
//!
//! Backed by a `ChannelRouter` lookup. The tool is mandatory in the
//! channel surface: an agent must be able to enumerate group members
//! before sending so policies can verify it is authorised.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::owner::Owner;
use crabgent_core::policy::{PolicyDecision, PolicyHook};
use crabgent_core::tool::{Tool, ToolCtx, parse_args};
use crabgent_core::types::ToolResult;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::action::channel_list_participants_action;
use crate::participant::Participant;
use crate::sink::ChannelRouter;

use super::{channel_error_to_tool_error, soft_result};

/// Tool the LLM calls to list participants in a conversation.
///
/// Schema: `{ "channel": "slack", "conv": "slack:T1/C1" }`. Returns a
/// JSON array of `{ id, role, display_name }` records.
pub struct ChannelListParticipantsTool {
    router: Arc<ChannelRouter>,
    policy: Arc<dyn PolicyHook>,
}

impl ChannelListParticipantsTool {
    /// Build a tool wrapping the given router.
    #[must_use]
    pub fn new(router: Arc<ChannelRouter>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { router, policy }
    }
}

#[derive(Debug, Deserialize)]
struct ListArgs {
    channel: String,
    conv: String,
}

fn render_participant(p: &Participant) -> Value {
    json!({
        "id": p.id.as_str(),
        "role": p.role.as_str(),
        "display_name": p.display_name,
    })
}

#[async_trait]
impl Tool for ChannelListParticipantsTool {
    fn name(&self) -> &'static str {
        "channel_list_participants"
    }
    fn description(&self) -> &'static str {
        "List the participants in a conversation. Specify `channel` \
         (the adapter name, e.g. \"slack\") and `conv` (the \
         conversation Owner string). Returns an array of \
         {id, role, display_name}."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["channel", "conv"],
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Adapter name, e.g. \"slack\"."
                },
                "conv": {
                    "type": "string",
                    "description": "Conversation owner string, e.g. \"slack:T1/C1\"."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let parsed: ListArgs = parse_args(args)?;
        let conv = Owner::new(parsed.conv);
        self.gate(ctx, &parsed.channel, &conv).await?;
        let channel = self
            .router
            .get(&parsed.channel)
            .ok_or_else(|| ToolError::NotFound(parsed.channel.clone()))?;
        let participants = channel
            .participants(&ctx.subject, &conv)
            .await
            .map_err(channel_error_to_tool_error)?;
        let arr: Vec<Value> = participants.iter().map(render_participant).collect();
        Ok(Value::Array(arr))
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        soft_result(self.execute(args, ctx).await)
    }
}

impl ChannelListParticipantsTool {
    async fn gate(&self, ctx: &ToolCtx, channel: &str, conv: &Owner) -> Result<(), ToolError> {
        let action = channel_list_participants_action(channel, conv);
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
    use crate::participant::ParticipantRole;
    use crate::test_support::RecordingChannel;
    use crabgent_core::policy::{AllowAllPolicy, DenyAllPolicy};
    use crabgent_core::subject::Subject;

    fn build_tool(participants: Vec<Participant>) -> ChannelListParticipantsTool {
        let stub: Arc<dyn Channel> = Arc::new(
            RecordingChannel::new("stub", ChannelKind::Group, "id").with_participants(participants),
        );
        let router = Arc::new(ChannelRouter::new().with_channel(stub));
        ChannelListParticipantsTool::new(router, Arc::new(AllowAllPolicy))
    }

    #[tokio::test]
    async fn list_tool_returns_array() {
        let tool = build_tool(vec![
            Participant::new("U1", ParticipantRole::Human).with_display_name("Alice"),
            Participant::new("B1", ParticipantRole::Bot),
        ]);
        let ctx = ToolCtx::new(Subject::new("agent"));
        let r = tool
            .execute(json!({"channel": "stub", "conv": "stub:c"}), &ctx)
            .await
            .expect("ok");
        let arr = r.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], "U1");
        assert_eq!(arr[0]["role"], "human");
        assert_eq!(arr[0]["display_name"], "Alice");
        assert_eq!(arr[1]["id"], "B1");
        assert_eq!(arr[1]["role"], "bot");
        assert!(arr[1]["display_name"].is_null());
    }

    #[tokio::test]
    async fn list_tool_unknown_channel_maps_to_not_found() {
        let tool = build_tool(vec![]);
        let ctx = ToolCtx::new(Subject::new("agent"));
        let err = tool
            .execute(json!({"channel": "nope", "conv": "c"}), &ctx)
            .await
            .expect_err("fail");
        match err {
            ToolError::NotFound(name) => assert_eq!(name, "nope"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_tool_invalid_args_returns_error() {
        let tool = build_tool(vec![]);
        let ctx = ToolCtx::new(Subject::new("agent"));
        let err = tool
            .execute(json!({"channel": 42}), &ctx)
            .await
            .expect_err("fail");
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn list_tool_denies_before_channel_lookup() {
        let stub: Arc<dyn Channel> =
            Arc::new(RecordingChannel::new("stub", ChannelKind::Group, "id"));
        let router = Arc::new(ChannelRouter::new().with_channel(stub));
        let tool = ChannelListParticipantsTool::new(router, Arc::new(DenyAllPolicy));
        let ctx = ToolCtx::new(Subject::new("agent"));
        let err = tool
            .execute(json!({"channel": "stub", "conv": "stub:c"}), &ctx)
            .await
            .expect_err("policy deny");
        assert!(matches!(err, ToolError::Permission(_)));
    }

    #[tokio::test]
    async fn list_tool_execute_result_wraps_policy_deny_as_soft_error() {
        let stub: Arc<dyn Channel> =
            Arc::new(RecordingChannel::new("stub", ChannelKind::Group, "id"));
        let router = Arc::new(ChannelRouter::new().with_channel(stub));
        let tool = ChannelListParticipantsTool::new(router, Arc::new(DenyAllPolicy));
        let ctx = ToolCtx::new(Subject::new("agent"));
        let result = tool
            .execute_result(json!({"channel": "stub", "conv": "stub:c"}), &ctx)
            .await
            .expect("soft wrap");
        assert!(result.is_error, "deny must produce soft error, not success");
    }

    #[test]
    fn list_tool_metadata_sane() {
        let tool = build_tool(vec![]);
        assert_eq!(tool.name(), "channel_list_participants");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "channel");
        assert_eq!(schema["required"][1], "conv");
    }
}
