use std::sync::Arc;

use async_trait::async_trait;
use crabgent_channel::{
    CHANNEL_LIST_PARTICIPANTS, CHANNEL_SEND, Channel, ChannelKind, ChannelListParticipantsTool,
    ChannelRouter, ChannelSendTool, ChannelSink, ChannelSubjectExt,
};
use crabgent_core::Action;
use crabgent_core::error::ToolError;
use crabgent_core::owner::Owner;
use crabgent_core::policy::{PolicyDecision, PolicyHook};
use crabgent_core::subject::Subject;
use crabgent_core::tool::{Tool, ToolCtx};
use serde_json::json;

mod support;

use support::RecordingChannel;

struct SameTargetPolicy;

#[async_trait]
impl PolicyHook for SameTargetPolicy {
    async fn allow(&self, subject: &Subject, action: &Action) -> PolicyDecision {
        let allowed = match action {
            Action::Targeted { name, target } if name == CHANNEL_SEND => {
                target_matches_subject(subject, target.qualifier(), target.owner())
            }
            Action::Targeted { name, target } if name == CHANNEL_LIST_PARTICIPANTS => {
                target_matches_subject(subject, target.qualifier(), target.owner())
            }
            _ => false,
        };
        if allowed {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny("channel target mismatch".into())
        }
    }
}

fn target_matches_subject(subject: &Subject, channel: Option<&str>, conv: &Owner) -> bool {
    subject.attr("channel") == channel && subject.attr("conv") == Some(conv.as_str())
}

fn subject_for(conv: &str) -> Subject {
    Subject::new("agent").with_channel("stub", &Owner::new(conv), ChannelKind::Group)
}

fn send_tool(stub: Arc<RecordingChannel>) -> ChannelSendTool {
    let channel: Arc<dyn Channel> = stub;
    let router = Arc::new(ChannelRouter::new().with_channel(channel));
    let sink: Arc<dyn ChannelSink> = router;
    ChannelSendTool::new(sink, Arc::new(SameTargetPolicy))
}

fn list_tool(stub: Arc<RecordingChannel>) -> ChannelListParticipantsTool {
    let channel: Arc<dyn Channel> = stub;
    let router = Arc::new(ChannelRouter::new().with_channel(channel));
    ChannelListParticipantsTool::new(router, Arc::new(SameTargetPolicy))
}

#[tokio::test]
async fn send_to_subject_conversation_is_allowed() {
    let stub = Arc::new(RecordingChannel::new());
    let tool = send_tool(Arc::clone(&stub));
    let ctx = ToolCtx::new(subject_for("stub:c1"));
    let result = tool
        .execute(json!({"conv": "stub:c1", "body": "hi"}), &ctx)
        .await
        .expect("send allowed");
    assert_eq!(result["id"], "ts:1");
    assert_eq!(stub.send_count(), 1);
}

#[tokio::test]
async fn send_to_other_conversation_is_denied_before_channel_call() {
    let stub = Arc::new(RecordingChannel::new());
    let tool = send_tool(Arc::clone(&stub));
    let ctx = ToolCtx::new(subject_for("stub:c1"));
    let err = tool
        .execute(json!({"conv": "stub:c2", "body": "hi"}), &ctx)
        .await
        .expect_err("send denied");
    assert!(matches!(err, ToolError::Permission(_)));
    assert_eq!(stub.send_count(), 0);
}

#[tokio::test]
async fn list_participants_for_subject_conversation_is_allowed() {
    let stub = Arc::new(RecordingChannel::new());
    let tool = list_tool(Arc::clone(&stub));
    let ctx = ToolCtx::new(subject_for("stub:c1"));
    let result = tool
        .execute(json!({"channel": "stub", "conv": "stub:c1"}), &ctx)
        .await
        .expect("list allowed");
    assert_eq!(result.as_array().expect("array").len(), 1);
    assert_eq!(stub.list_count(), 1);
}

#[tokio::test]
async fn list_participants_for_other_conversation_is_denied_before_channel_call() {
    let stub = Arc::new(RecordingChannel::new());
    let tool = list_tool(Arc::clone(&stub));
    let ctx = ToolCtx::new(subject_for("stub:c1"));
    let err = tool
        .execute(json!({"channel": "stub", "conv": "stub:c2"}), &ctx)
        .await
        .expect_err("list denied");
    assert!(matches!(err, ToolError::Permission(_)));
    assert_eq!(stub.list_count(), 0);
}
