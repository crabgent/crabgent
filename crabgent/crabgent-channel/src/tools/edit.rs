//! `ChannelEditTool`: tool the LLM uses to edit a message via a
//! `ChannelSink`.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::owner::Owner;
use crabgent_core::policy::PolicyHook;
use crabgent_core::tool::{Tool, ToolCtx, parse_args};
use crabgent_core::types::ToolResult;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::sink::ChannelSink;

use super::{
    MessageRefLocation, channel_error_to_tool_error, execute_result_with_outbound, gate_tool,
    message_ref_from_id,
};

const TOOL_NAME: &str = "channel_edit";

/// Tool the LLM calls to edit an existing channel message.
pub struct ChannelEditTool {
    sink: Arc<dyn ChannelSink>,
    policy: Arc<dyn PolicyHook>,
}

impl ChannelEditTool {
    /// Build a tool wrapping the given sink.
    #[must_use]
    pub fn new(sink: Arc<dyn ChannelSink>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { sink, policy }
    }
}

#[derive(Debug, Deserialize)]
struct EditArgs {
    conv: String,
    id: String,
    new_text: String,
    #[serde(default)]
    thread_root: Option<String>,
    #[serde(default)]
    broadcast: bool,
}

#[async_trait]
impl Tool for ChannelEditTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Edit a message via a channel adapter. Args: conv, id, new_text, \
         optional thread_root, optional broadcast. Message ids are \
         channel-opaque strings."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["conv", "id", "new_text"],
            "properties": {
                "conv": {
                    "type": "string",
                    "description": "Conversation owner string, e.g. \"slack:T1/C1\"."
                },
                "id": {
                    "type": "string",
                    "description": "Channel-opaque message id."
                },
                "new_text": {
                    "type": "string",
                    "description": "Replacement message text."
                },
                "thread_root": {
                    "type": "string",
                    "description": "Optional channel-opaque thread root id."
                },
                "broadcast": {
                    "type": "boolean",
                    "default": false
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: EditArgs = parse_args(args)?;
        let conv = Owner::new(args.conv);
        let target = message_ref_from_id(&conv, args.id, args.thread_root, args.broadcast)?;
        gate_tool(self.policy.as_ref(), ctx, TOOL_NAME).await?;
        self.sink
            .edit(&ctx.subject, &conv, &target, &args.new_text)
            .await
            .map_err(channel_error_to_tool_error)?;
        Ok(json!({
            "ok": true,
            "message": super::render_message_ref(&target),
        }))
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        execute_result_with_outbound(
            self,
            args,
            ctx,
            "new_text",
            MessageRefLocation::Field("message"),
        )
        .await
    }
}
