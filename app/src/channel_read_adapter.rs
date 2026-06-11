//! Local wrapper around upstream `ChannelReadTool`.
//!
//! Sole purpose: override the LLM-facing `description` for this
//! deployment. Everything else (schema, gating, execute) delegates to
//! the upstream tool, so this stays a thin adapter.

use async_trait::async_trait;
use crabgent_channel::ChannelReadTool;
use crabgent_core::ToolResult;
use crabgent_core::error::ToolError;
use crabgent_core::tool::{Tool, ToolCtx};
use serde_json::Value;

pub struct ChannelReadAdapter {
    inner: ChannelReadTool,
}

impl ChannelReadAdapter {
    pub const fn new(inner: ChannelReadTool) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Tool for ChannelReadAdapter {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn description(&self) -> &'static str {
        "Read recent messages via a channel adapter. Args: conv, optional \
         thread_parent, optional limit. Message ids are channel-opaque \
         strings. This reads Matrix, Telegram, and active TUI conversations; \
         tmux is not a channel adapter. Trusted local agents may have a \
         separate `tmux` tool for explicit user-requested pane inspection or \
         posting."
    }

    fn parameters_schema(&self) -> Value {
        self.inner.parameters_schema()
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        self.inner.execute(args, ctx).await
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        self.inner.execute_result(args, ctx).await
    }
}
