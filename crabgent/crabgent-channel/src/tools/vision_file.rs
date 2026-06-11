//! `VisionFileTool`: attach a local image file to the current run as a
//! provider-neutral vision message.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::message::{ContentBlock, ImagePayload, Message};
use crabgent_core::policy::PolicyHook;
use crabgent_core::tool::{Tool, ToolCtx, parse_args};
use crabgent_core::types::ToolResult;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::image_validation::{ImageValidator, MAX_IMAGE_BYTES, mime_from_image_bytes};

use super::gate_tool;

const TOOL_NAME: &str = "vision_file";

/// Tool the LLM calls to attach a local image file for the next vision turn.
pub struct VisionFileTool {
    policy: Arc<dyn PolicyHook>,
    validator: ImageValidator,
}

impl VisionFileTool {
    /// Build a local vision file tool.
    #[must_use]
    pub fn new(policy: Arc<dyn PolicyHook>) -> Self {
        Self {
            policy,
            validator: ImageValidator::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Args {
    path: PathBuf,
    #[serde(default)]
    question: Option<String>,
}

#[async_trait]
impl Tool for VisionFileTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Attach a local image file to the current run as a real vision input. \
         Args: path, optional question. Use this for local PNG/JPEG/GIF/WebP \
         files that should be analyzed by a vision-capable model. Do not read \
         or base64-print the image first; this tool injects the image bytes as \
         an ImagePayload for the next model turn."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Local image path on the agent host."
                },
                "question": {
                    "type": "string",
                    "description": "Optional specific question or instruction for analyzing this image."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let result = self.execute_result(args, ctx).await?;
        if result.is_error {
            return Ok(result.output);
        }
        Ok(result.output)
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        let args: Args = parse_args(args)?;
        gate_tool(self.policy.as_ref(), ctx, TOOL_NAME).await?;
        match build_image_message(&self.validator, &args.path, args.question.as_deref()) {
            Ok((message, output)) => Ok(ToolResult::success(output).with_run_message(message)),
            Err(ToolError::Permission(reason) | ToolError::InvalidArgs(reason)) => {
                Ok(ToolResult::soft_error(json!(reason)))
            }
            Err(ToolError::Cancelled) => Err(ToolError::Cancelled),
            Err(error) => Ok(ToolResult::soft_error(json!(error.to_string()))),
        }
    }
}

fn build_image_message(
    validator: &ImageValidator,
    path: &Path,
    question: Option<&str>,
) -> Result<(Message, Value), ToolError> {
    let meta = std::fs::metadata(path)
        .map_err(|error| ToolError::InvalidArgs(format!("path: {error}")))?;
    if !meta.is_file() {
        return Err(ToolError::InvalidArgs("path is not a file".to_owned()));
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "image size {} exceeds {MAX_IMAGE_BYTES} byte limit",
            meta.len()
        )));
    }

    let bytes =
        std::fs::read(path).map_err(|error| ToolError::InvalidArgs(format!("path: {error}")))?;
    let detected_mime = mime_from_image_bytes(&bytes)
        .map_err(|rejection| ToolError::InvalidArgs(format!("image rejected: {rejection}")))?;
    let mime = validator
        .validate(&bytes, detected_mime)
        .map_err(|rejection| ToolError::InvalidArgs(format!("image rejected: {rejection}")))?;
    let payload = ImagePayload::new(bytes, mime.to_owned())
        .map_err(|error| ToolError::InvalidArgs(format!("image payload: {error}")))?;

    let mut text = format!(
        "Local image attached by `vision_file`.\nPath: {}",
        path.display()
    );
    if let Some(question) = question
        .map(str::trim)
        .filter(|question| !question.is_empty())
    {
        text.push_str("\nQuestion: ");
        text.push_str(question);
    } else {
        text.push_str("\nUse this image to answer the user's request.");
    }

    let size_bytes = payload.bytes().len();
    let message = Message::user(vec![
        ContentBlock::Text { text },
        ContentBlock::Image(payload),
    ]);
    let output = json!({
        "ok": true,
        "path": path.display().to_string(),
        "mime": mime,
        "size_bytes": size_bytes,
        "injected": true
    });
    Ok((message, output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::policy::AllowAllPolicy;
    use crabgent_core::subject::Subject;
    use tempfile::NamedTempFile;

    fn ctx() -> ToolCtx {
        ToolCtx::new(Subject::new("agent"))
    }

    fn policy() -> Arc<dyn PolicyHook> {
        Arc::new(AllowAllPolicy)
    }

    fn minimal_png_bytes() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00,
        ]
    }

    #[test]
    fn schema_requires_path() {
        let tool = VisionFileTool::new(policy());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["path"]));
        assert!(schema["properties"]["question"].is_object());
    }

    #[tokio::test]
    async fn injects_image_run_message() {
        let mut file = NamedTempFile::new().expect("temp file");
        std::io::Write::write_all(&mut file, &minimal_png_bytes()).expect("write png");
        let tool = VisionFileTool::new(policy());

        let result = tool
            .execute_result(
                json!({
                    "path": file.path().to_string_lossy(),
                    "question": "What is in this image?"
                }),
                &ctx(),
            )
            .await
            .expect("tool result");

        assert!(!result.is_error);
        assert_eq!(result.output["ok"], true);
        assert_eq!(result.output["mime"], "image/png");
        assert_eq!(result.run_messages.len(), 1);
        let Message::User { content, .. } = &result.run_messages[0] else {
            panic!("expected injected user message");
        };
        assert!(matches!(content.first(), Some(ContentBlock::Text { .. })));
        assert!(matches!(content.get(1), Some(ContentBlock::Image(_))));
    }

    #[tokio::test]
    async fn invalid_image_is_soft_error_without_run_message() {
        let mut file = NamedTempFile::new().expect("temp file");
        std::io::Write::write_all(&mut file, b"not an image").expect("write bytes");
        let tool = VisionFileTool::new(policy());

        let result = tool
            .execute_result(json!({"path": file.path().to_string_lossy()}), &ctx())
            .await
            .expect("tool result");

        assert!(result.is_error);
        assert!(result.output.to_string().contains("image rejected"));
        assert!(result.run_messages.is_empty());
    }
}
