use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{Tool, ToolCtx, ToolError, ToolResult};
use serde_json::Value;

use crate::{McpClient, McpError};

mod factory;

pub use factory::McpToolFactory;

pub struct McpTool {
    pub(crate) prefixed_name: &'static str,
    // Initial: bare String, Newtype pending
    pub(crate) original_name: String,
    pub(crate) description: &'static str,
    pub(crate) input_schema: Value,
    pub(crate) client: Arc<McpClient>,
    pub(crate) max_output_bytes: usize,
}

impl McpTool {
    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<Value, McpError> {
        let result = self
            .client
            .call_tool(&self.original_name, args, ctx.cancel.as_ref())
            .await?;
        let text = match result.content {
            Value::String(text) => text,
            other => other.to_string(),
        };
        Ok(Value::String(
            crabgent_core::text::truncate_bytes_at_boundary(&text, self.max_output_bytes)
                .to_owned(),
        ))
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &'static str {
        self.prefixed_name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn parameters_schema(&self) -> Value {
        self.input_schema.clone()
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        self.call(args, ctx).await.map_err(|err| tool_error(&err))
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        Ok(match self.call(args, ctx).await {
            Ok(value) => ToolResult::success(value),
            Err(err) => ToolResult::soft_error(Value::String(tool_error_message(&err))),
        })
    }
}

fn tool_error(err: &McpError) -> ToolError {
    ToolError::Execution(tool_error_message(err))
}

fn tool_error_message(err: &McpError) -> String {
    match err {
        McpError::AuthFailed => "MCP server authentication failed".to_string(),
        McpError::Cancelled => "cancelled".to_string(),
        McpError::JsonRpc { code, message } => format!(
            "MCP JSON-RPC error {code}: {}",
            crabgent_log::redact_text(message)
        ),
        other => other.to_string(),
    }
}
