//! Tool the LLM calls to generate an image via an `ImageGenerationProvider`
//! and upload it to the current conversation through `ChannelSink`.
//!
//! Synthesises a fresh `RunCtx` from the tool-side `ToolCtx` because the
//! provider trait demands `&RunCtx` for auth header plumbing while
//! `ToolCtx` only carries the parent `Subject` and a cancellation token.
//! The synthetic `run_id` only affects opaque correlation headers, never
//! the run loop the kernel drives.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::ChannelSink;
use crabgent_core::owner::Owner;
use crabgent_core::{
    Action, ImageGenerationProvider, ImageGenerationQuality, ImageGenerationRequest,
    ImageGenerationSize, PolicyDecision, PolicyHook, RunCtx, RunId, Tool, ToolCtx, ToolError,
    ToolResult,
};
use serde::Deserialize;
use serde_json::{Value, json};

const TOOL_NAME: &str = "generate_image";
const DEFAULT_MODEL: &str = "gpt-image-1.5";

pub struct GenerateImageTool {
    provider: Arc<dyn ImageGenerationProvider>,
    sink: Arc<dyn ChannelSink>,
    policy: Arc<dyn PolicyHook>,
    default_model: String,
}

impl GenerateImageTool {
    #[must_use]
    pub fn new(
        provider: Arc<dyn ImageGenerationProvider>,
        sink: Arc<dyn ChannelSink>,
        policy: Arc<dyn PolicyHook>,
    ) -> Self {
        Self {
            provider,
            sink,
            policy,
            default_model: DEFAULT_MODEL.to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Args {
    prompt: String,
    conv: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    quality: Option<String>,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    thread_parent: Option<String>,
}

#[async_trait]
impl Tool for GenerateImageTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Generate an image with an OpenAI gpt-image model and upload it \
         to the conversation. Required: `prompt`, `conv` (conversation owner \
         string, same shape as channel_send). Optional: `model` (default \
         gpt-image-1.5; alternatives gpt-image-2, gpt-image-1, gpt-image-1-mini), \
         `size` (e.g. \"1024x1024\", \"1024x1536\", \"1536x1024\"), `quality` \
         (low|medium|high|auto), `comment` (caption posted with the image), \
         `thread_parent` (channel-opaque parent id, defaults to the inbound \
         thread). Returns ok=true with the message ref on success."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["prompt", "conv"],
            "properties": {
                "prompt": {"type": "string"},
                "conv": {"type": "string", "description": "Conversation owner string."},
                "model": {"type": ["string", "null"]},
                "size": {"type": ["string", "null"], "description": "e.g. 1024x1024."},
                "quality": {
                    "type": ["string", "null"],
                    "enum": ["low", "medium", "high", "auto", null]
                },
                "comment": {"type": ["string", "null"]},
                "thread_parent": {"type": ["string", "null"]}
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: Args = crabgent_core::tool::parse_args(args)?;
        match self
            .policy
            .allow(&ctx.subject, &Action::ToolCall(TOOL_NAME.into()))
            .await
        {
            PolicyDecision::Allow => {}
            PolicyDecision::Deny(reason) => {
                return Err(ToolError::Permission(reason));
            }
        }

        let model = args.model.unwrap_or_else(|| self.default_model.clone());
        let mut req = ImageGenerationRequest::new(model.clone(), args.prompt);
        if let Some(s) = args.size {
            req.size = Some(ImageGenerationSize::from(s));
        }
        if let Some(q) = args.quality.as_deref() {
            req.quality = parse_quality(q)?;
        }

        let mut run_ctx = RunCtx::new(RunId::new(), ctx.subject.clone());
        if let Some(cancel) = &ctx.cancel {
            run_ctx = run_ctx.with_cancel(cancel.clone());
        }

        let resp = self
            .provider
            .generate_image(req, &run_ctx, ctx.cancel.as_ref())
            .await
            .map_err(|err| ToolError::Execution(format!("image generation: {err}")))?;

        let image = resp
            .images
            .first()
            .ok_or_else(|| ToolError::Execution("provider returned no images".to_owned()))?;
        let bytes = image.bytes().as_ref().to_vec();
        let ext = mime_to_ext(image.mime());
        let filename = format!("generated-{}.{ext}", Utc::now().format("%Y%m%d-%H%M%S"));
        let conv = Owner::new(args.conv);

        let message_ref = self
            .sink
            .upload(
                &ctx.subject,
                &conv,
                &filename,
                bytes,
                args.comment.as_deref(),
                None,
            )
            .await
            .map_err(|err| ToolError::Execution(format!("channel upload: {err}")))?;

        Ok(json!({
            "ok": true,
            "model": resp.model.as_str(),
            "filename": filename,
            "bytes": image.bytes().len(),
            "mime": image.mime(),
            "revised_prompt": image.revised_prompt.clone(),
            "message": format!("{}:{}", message_ref.conv.as_str(), message_ref.id.as_str()),
            "thread_parent_hint": args.thread_parent,
        }))
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        match self.execute(args, ctx).await {
            Ok(output) => Ok(ToolResult::success(output)),
            Err(err) => Ok(ToolResult::soft_error(json!({"error": err.to_string()}))),
        }
    }
}

fn parse_quality(raw: &str) -> Result<Option<ImageGenerationQuality>, ToolError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Ok(Some(ImageGenerationQuality::Auto)),
        "low" => Ok(Some(ImageGenerationQuality::Low)),
        "medium" => Ok(Some(ImageGenerationQuality::Medium)),
        "high" => Ok(Some(ImageGenerationQuality::High)),
        other => Err(ToolError::InvalidArgs(format!(
            "quality: expected low|medium|high|auto, got {other:?}"
        ))),
    }
}

fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}
