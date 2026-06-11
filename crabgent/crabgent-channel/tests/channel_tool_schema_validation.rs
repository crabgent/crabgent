use std::io::Write as _;
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
use serde_json::{Value, json};

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

fn required(schema: &Value) -> Vec<&str> {
    schema["required"]
        .as_array()
        .expect("required array")
        .iter()
        .map(|value| value.as_str().expect("required item"))
        .collect()
}

#[test]
fn channel_edit_schema_lists_required_args() {
    let tool = ChannelEditTool::new(sink(), policy());
    let schema = tool.parameters_schema();
    assert_eq!(schema["type"], "object");
    assert_eq!(required(&schema), vec!["conv", "id", "new_text"]);
    assert!(schema["properties"]["new_text"].is_object());
}

#[test]
fn channel_delete_schema_lists_required_args() {
    let tool = ChannelDeleteTool::new(sink(), policy());
    let schema = tool.parameters_schema();
    assert_eq!(schema["type"], "object");
    assert_eq!(required(&schema), vec!["conv", "id"]);
    assert!(schema["properties"]["id"].is_object());
}

#[test]
fn channel_upload_schema_lists_required_args() {
    let tool = ChannelUploadTool::new(sink(), policy());
    let schema = tool.parameters_schema();
    assert_eq!(schema["type"], "object");
    assert_eq!(required(&schema), vec!["conv", "filename"]);
    assert!(schema["properties"]["content_base64"].is_object());
    assert!(schema["properties"]["path"].is_object());
    assert!(schema["properties"]["comment"].is_object());
}

#[test]
fn channel_read_schema_lists_required_args() {
    let tool = ChannelReadTool::new(sink(), policy());
    let schema = tool.parameters_schema();
    assert_eq!(schema["type"], "object");
    assert_eq!(required(&schema), vec!["conv"]);
    assert_eq!(schema["properties"]["limit"]["default"], 20);
}

#[tokio::test]
async fn channel_edit_args_parse_before_sink_unsupported() {
    let tool = ChannelEditTool::new(sink(), policy());
    let result = tool
        .execute_result(
            json!({"conv": "stub:c", "id": "m1", "new_text": "updated"}),
            &ctx(),
        )
        .await
        .expect("soft result");
    assert!(result.is_error);
    assert!(result.output.to_string().contains("edit"));
}

#[tokio::test]
async fn channel_delete_args_parse_before_sink_unsupported() {
    let tool = ChannelDeleteTool::new(sink(), policy());
    let result = tool
        .execute_result(json!({"conv": "stub:c", "id": "m1"}), &ctx())
        .await
        .expect("soft result");
    assert!(result.is_error);
    assert!(result.output.to_string().contains("delete"));
}

#[tokio::test]
async fn channel_upload_args_parse_and_decode_before_sink_unsupported() {
    let tool = ChannelUploadTool::new(sink(), policy());
    let result = tool
        .execute_result(
            json!({"conv": "stub:c", "filename": "a.txt", "content_base64": "aGk="}),
            &ctx(),
        )
        .await
        .expect("soft result");
    assert!(result.is_error);
    assert!(result.output.to_string().contains("upload"));
}

#[tokio::test]
async fn channel_upload_accepts_wrapped_base64() {
    let tool = ChannelUploadTool::new(sink(), policy());
    let result = tool
        .execute_result(
            json!({"conv": "stub:c", "filename": "a.txt", "content_base64": "a\nG\tk=\r\n"}),
            &ctx(),
        )
        .await
        .expect("soft result");
    assert!(result.is_error);
    assert!(result.output.to_string().contains("upload"));
}

#[tokio::test]
async fn channel_upload_accepts_local_path() {
    let mut file = tempfile::NamedTempFile::new().expect("temp file");
    file.write_all(b"hi").expect("write temp");
    let tool = ChannelUploadTool::new(sink(), policy());
    let result = tool
        .execute_result(
            json!({
                "conv": "stub:c",
                "filename": "a.txt",
                "path": file.path().to_string_lossy()
            }),
            &ctx(),
        )
        .await
        .expect("soft result");
    assert!(result.is_error);
    assert!(result.output.to_string().contains("upload"));
}

#[tokio::test]
async fn channel_upload_rejects_path_and_base64_together() {
    let mut file = tempfile::NamedTempFile::new().expect("temp file");
    file.write_all(b"hi").expect("write temp");
    let tool = ChannelUploadTool::new(sink(), policy());
    let result = tool
        .execute_result(
            json!({
                "conv": "stub:c",
                "filename": "a.txt",
                "path": file.path().to_string_lossy(),
                "content_base64": "aGk="
            }),
            &ctx(),
        )
        .await
        .expect("soft result");
    assert!(result.is_error);
    assert!(
        result
            .output
            .to_string()
            .contains("provide either path or content_base64")
    );
}

#[tokio::test]
async fn channel_read_args_parse_before_sink_unsupported() {
    let tool = ChannelReadTool::new(sink(), policy());
    let result = tool
        .execute_result(
            json!({"conv": "stub:c", "thread_parent": "m1", "limit": 5}),
            &ctx(),
        )
        .await
        .expect("soft result");
    assert!(result.is_error);
    assert!(result.output.to_string().contains("read"));
}
