//! Wrapper helpers for exposing `Tool` implementations as commands.

use std::sync::Arc;

use crabgent_core::{Tool, ToolCtx, ToolResult};
use serde_json::Value;

use crate::command::CommandCtx;
use crate::error::CommandError;

/// Executes a `Tool` from a command context.
pub struct ToolCommand {
    tool: Arc<dyn Tool>,
}

impl ToolCommand {
    /// Build a tool wrapper.
    #[must_use]
    pub fn new(tool: Arc<dyn Tool>) -> Self {
        Self { tool }
    }

    /// Execute the wrapped tool with a `ToolCtx` derived from `CommandCtx`.
    pub async fn execute(&self, args: Value, ctx: &CommandCtx) -> Result<ToolResult, CommandError> {
        let mut tool_ctx =
            ToolCtx::new(ctx.subject().clone()).with_session_id(ctx.session_id().to_string());
        if let Some(cancel) = ctx.cancel() {
            tool_ctx = tool_ctx.with_cancel(cancel.clone());
        }
        self.tool
            .execute_result(args, &tool_ctx)
            .await
            .map_err(CommandError::from)
    }
}

/// Convert a tool output value into command reply text.
#[must_use]
pub fn stringify_tool_output(output: &Value) -> String {
    match output {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

#[cfg(test)]
mod tests;
