//! `ChannelReadTool`: tool the LLM uses to read messages via a
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

use crate::channel::ReadMessage;
use crate::envelope::MessageRef;
use crate::sink::ChannelSink;

use super::{
    channel_error_to_tool_error, gate_tool, message_ref_from_id, render_message_ref, soft_result,
};

const TOOL_NAME: &str = "channel_read";
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

/// Tool the LLM calls to read recent messages from a channel.
pub struct ChannelReadTool {
    sink: Arc<dyn ChannelSink>,
    policy: Arc<dyn PolicyHook>,
}

impl ChannelReadTool {
    /// Build a tool wrapping the given sink.
    #[must_use]
    pub fn new(sink: Arc<dyn ChannelSink>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { sink, policy }
    }
}

#[derive(Debug, Deserialize)]
struct ReadArgs {
    conv: String,
    #[serde(default)]
    thread_parent: Option<String>,
    #[serde(default = "default_limit")]
    limit: u64,
}

const fn default_limit() -> u64 {
    20
}

#[async_trait]
impl Tool for ChannelReadTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Read recent messages via a channel adapter. Args: conv, optional \
         thread_parent, optional limit. Message ids are channel-opaque \
         strings."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["conv"],
            "properties": {
                "conv": {
                    "type": "string",
                    "description": "Conversation owner string, e.g. \"slack:T1/C1\"."
                },
                "thread_parent": {
                    "type": "string",
                    "description": "Optional channel-opaque parent message id."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "default": DEFAULT_LIMIT,
                    "maximum": MAX_LIMIT
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: ReadArgs = parse_args(args)?;
        let conv = Owner::new(args.conv);
        let thread_parent = thread_parent_ref(&conv, args.thread_parent)?;
        gate_tool(self.policy.as_ref(), ctx, TOOL_NAME).await?;
        let messages = self
            .sink
            .read(
                &ctx.subject,
                &conv,
                thread_parent.as_ref(),
                limit(args.limit),
            )
            .await
            .map_err(channel_error_to_tool_error)?;
        Ok(render_messages(&messages, thread_parent.as_ref()))
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        soft_result(self.execute(args, ctx).await)
    }
}

fn limit(limit: u64) -> usize {
    usize::try_from(limit)
        .unwrap_or(MAX_LIMIT)
        .clamp(1, MAX_LIMIT)
}

fn thread_parent_ref(
    conv: &Owner,
    thread_parent: Option<String>,
) -> Result<Option<MessageRef>, ToolError> {
    thread_parent
        .map(|id| message_ref_from_id(conv, id, None, false))
        .transpose()
}

fn render_messages(messages: &[ReadMessage], thread_parent: Option<&MessageRef>) -> Value {
    let messages = messages
        .iter()
        .map(|message| {
            json!({
                "message_ref": render_message_ref(&message.message_ref),
                "author": message.author.as_str(),
                "body": message.body.as_str(),
                "timestamp_unix_ms": message.timestamp_unix_ms,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "messages": messages,
        "thread_parent": thread_parent.map(render_message_ref),
    })
}
