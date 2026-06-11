use std::sync::Arc;

use async_trait::async_trait;
use crabgent_channel::{
    ChannelDeleteTool, ChannelEditTool, ChannelError, ChannelReadTool, ChannelSink,
    ChannelUploadTool, MessageRef, OutboundMessage,
};
use crabgent_core::owner::Owner;
use crabgent_core::policy::{AllowAllPolicy, PolicyHook};
use crabgent_core::subject::Subject;
use crabgent_core::tool::{Tool, ToolCtx};
use serde_json::json;

struct UnsupportedSink;

#[async_trait]
impl ChannelSink for UnsupportedSink {
    async fn send(
        &self,
        _ctx: &Subject,
        _conv: &Owner,
        _msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        Err(ChannelError::Unsupported("send"))
    }

    async fn react(
        &self,
        _ctx: &Subject,
        _conv: &Owner,
        _parent: &MessageRef,
        _emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        Err(ChannelError::Unsupported("react"))
    }
}

fn sink() -> Arc<dyn ChannelSink> {
    Arc::new(UnsupportedSink)
}

fn policy() -> Arc<dyn PolicyHook> {
    Arc::new(AllowAllPolicy)
}

fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("agent"))
}

fn assert_soft_unsupported(result: &crabgent_core::ToolResult, op: &str) {
    assert!(result.is_error);
    assert!(result.output.to_string().contains(op));
}

#[tokio::test]
async fn channel_edit_default_unsupported_returns_soft_error() {
    let tool = ChannelEditTool::new(sink(), policy());
    let result = tool
        .execute_result(
            json!({"conv": "stub:c", "id": "m1", "new_text": "updated"}),
            &ctx(),
        )
        .await
        .expect("soft result");
    assert_soft_unsupported(&result, "edit");
}

#[tokio::test]
async fn channel_delete_default_unsupported_returns_soft_error() {
    let tool = ChannelDeleteTool::new(sink(), policy());
    let result = tool
        .execute_result(json!({"conv": "stub:c", "id": "m1"}), &ctx())
        .await
        .expect("soft result");
    assert_soft_unsupported(&result, "delete");
}

#[tokio::test]
async fn channel_upload_default_unsupported_returns_soft_error() {
    let tool = ChannelUploadTool::new(sink(), policy());
    let result = tool
        .execute_result(
            json!({"conv": "stub:c", "filename": "a.txt", "content_base64": "aGk="}),
            &ctx(),
        )
        .await
        .expect("soft result");
    assert_soft_unsupported(&result, "upload");
}

#[tokio::test]
async fn channel_read_default_unsupported_returns_soft_error() {
    let tool = ChannelReadTool::new(sink(), policy());
    let result = tool
        .execute_result(json!({"conv": "stub:c"}), &ctx())
        .await
        .expect("soft result");
    assert_soft_unsupported(&result, "read");
}
