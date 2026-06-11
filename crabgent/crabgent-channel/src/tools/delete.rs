//! `ChannelDeleteTool`: tool the LLM uses to delete a message via a
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
    channel_error_to_tool_error, gate_tool, message_ref_from_id, render_message_ref, soft_result,
};

const TOOL_NAME: &str = "channel_delete";

/// Tool the LLM calls to delete an existing channel message.
pub struct ChannelDeleteTool {
    sink: Arc<dyn ChannelSink>,
    policy: Arc<dyn PolicyHook>,
}

impl ChannelDeleteTool {
    /// Build a tool wrapping the given sink.
    #[must_use]
    pub fn new(sink: Arc<dyn ChannelSink>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { sink, policy }
    }
}

#[derive(Debug, Deserialize)]
struct DeleteArgs {
    conv: String,
    id: String,
    #[serde(default)]
    thread_root: Option<String>,
    #[serde(default)]
    broadcast: bool,
}

#[async_trait]
impl Tool for ChannelDeleteTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Delete a message via a channel adapter. Args: conv, id, optional \
         thread_root, optional broadcast. Message ids are channel-opaque \
         strings."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["conv", "id"],
            "properties": {
                "conv": {
                    "type": "string",
                    "description": "Conversation owner string, e.g. \"slack:T1/C1\"."
                },
                "id": {
                    "type": "string",
                    "description": "Channel-opaque message id."
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
        let args: DeleteArgs = parse_args(args)?;
        let conv = Owner::new(args.conv);
        let target = message_ref_from_id(&conv, args.id, args.thread_root, args.broadcast)?;
        gate_tool(self.policy.as_ref(), ctx, TOOL_NAME).await?;
        self.sink
            .delete(&ctx.subject, &conv, &target)
            .await
            .map_err(channel_error_to_tool_error)?;
        Ok(json!({
            "ok": true,
            "message": render_message_ref(&target),
        }))
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        soft_result(self.execute(args, ctx).await)
    }
}
