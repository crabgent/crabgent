//! `ChannelUploadTool`: tool the LLM uses to upload bytes via a
//! `ChannelSink`.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use crabgent_core::error::ToolError;
use crabgent_core::owner::Owner;
use crabgent_core::policy::PolicyHook;
use crabgent_core::tool::{Tool, ToolCtx, parse_args};
use crabgent_core::types::ToolResult;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::envelope::MessageRef;
use crate::sink::ChannelSink;

use super::{
    channel_error_to_tool_error, gate_tool, message_ref_from_id, render_message_ref, soft_result,
};

const TOOL_NAME: &str = "channel_upload";
const MAX_UPLOAD_BYTES: usize = 50 * 1024 * 1024;

/// Tool the LLM calls to upload file bytes to a channel conversation.
pub struct ChannelUploadTool {
    sink: Arc<dyn ChannelSink>,
    policy: Arc<dyn PolicyHook>,
}

impl ChannelUploadTool {
    /// Build a tool wrapping the given sink.
    #[must_use]
    pub fn new(sink: Arc<dyn ChannelSink>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { sink, policy }
    }
}

#[derive(Debug, Deserialize)]
struct UploadArgs {
    conv: String,
    filename: String,
    #[serde(default)]
    content_base64: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    thread_parent: Option<String>,
}

#[async_trait]
impl Tool for ChannelUploadTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Upload a file via a channel adapter. Args: conv, filename, either \
         content_base64 or path, optional comment, optional thread_parent. \
         Prefer path for local files so large bytes do not enter the LLM \
         context. Rejects content over 50MB."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["conv", "filename"],
            "properties": {
                "conv": {
                    "type": "string",
                    "description": "Conversation owner string, e.g. \"slack:T1/C1\"."
                },
                "filename": {
                    "type": "string"
                },
                "content_base64": {
                    "type": "string",
                    "description": "Base64 file content supplied as tool-wire input."
                },
                "path": {
                    "type": "string",
                    "description": "Local file path to upload. Prefer this for files already on the host."
                },
                "comment": {
                    "type": "string",
                    "description": "Optional upload comment or caption."
                },
                "thread_parent": {
                    "type": "string",
                    "description": "Optional channel-opaque parent message id."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: UploadArgs = parse_args(args)?;
        let conv = Owner::new(args.conv.clone());
        let thread_parent = thread_parent_ref(&conv, args.thread_parent.clone())?;
        gate_tool(self.policy.as_ref(), ctx, TOOL_NAME).await?;
        let bytes = upload_bytes(&args)?;
        if bytes.len() > MAX_UPLOAD_BYTES {
            return Err(ToolError::InvalidArgs(format!(
                "upload size {} exceeds 50MB limit",
                bytes.len()
            )));
        }
        let result = self
            .sink
            .upload(
                &ctx.subject,
                &conv,
                &args.filename,
                bytes,
                args.comment.as_deref(),
                thread_parent.as_ref(),
            )
            .await
            .map_err(channel_error_to_tool_error)?;
        Ok(json!({
            "ok": true,
            "message": render_message_ref(&result),
        }))
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        soft_result(self.execute(args, ctx).await)
    }
}

fn upload_bytes(args: &UploadArgs) -> Result<Vec<u8>, ToolError> {
    if args.path.is_some() && !args.content_base64.is_empty() {
        return Err(ToolError::InvalidArgs(
            "provide either path or content_base64, not both".to_owned(),
        ));
    }
    if let Some(path) = args.path.as_deref() {
        return read_upload_path(path);
    }
    if args.content_base64.is_empty() {
        return Err(ToolError::InvalidArgs(
            "missing path or content_base64".to_owned(),
        ));
    }
    // wire-input decode, no vision path.
    let content_base64 = strip_ascii_whitespace(&args.content_base64);
    STANDARD
        .decode(content_base64.as_bytes())
        .map_err(|error| ToolError::InvalidArgs(format!("content_base64: {error}")))
}

fn read_upload_path(path: &str) -> Result<Vec<u8>, ToolError> {
    let meta = std::fs::metadata(path)
        .map_err(|error| ToolError::InvalidArgs(format!("path: {error}")))?;
    if !meta.is_file() {
        return Err(ToolError::InvalidArgs("path is not a file".to_owned()));
    }
    if meta.len() > MAX_UPLOAD_BYTES as u64 {
        return Err(ToolError::InvalidArgs(format!(
            "upload size {} exceeds 50MB limit",
            meta.len()
        )));
    }
    std::fs::read(Path::new(path)).map_err(|error| ToolError::InvalidArgs(format!("path: {error}")))
}

fn strip_ascii_whitespace(input: &str) -> String {
    input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .map(char::from)
        .collect()
}

fn thread_parent_ref(
    conv: &Owner,
    thread_parent: Option<String>,
) -> Result<Option<MessageRef>, ToolError> {
    thread_parent
        .map(|id| message_ref_from_id(conv, id, None, false))
        .transpose()
}
