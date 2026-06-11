use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::policy::PolicyHook;
use crabgent_core::tool::{Tool, ToolCtx, parse_args};
use crabgent_core::types::ToolResult;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::SlackHttpClient;

use super::{gate_tool, slack_error_to_tool_error, soft_result};

const TOOL_NAME: &str = "slack_search";
const MAX_RESULTS: usize = 20;
const MAX_RESULTS_U32: u32 = 20;
const MAX_TEXT_CHARS: usize = 300;

/// Search Slack messages.
pub struct SlackSearchTool {
    client: Arc<SlackHttpClient>,
    policy: Arc<dyn PolicyHook>,
}

impl SlackSearchTool {
    #[must_use]
    pub fn new(client: Arc<SlackHttpClient>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { client, policy }
    }
}

#[derive(Debug, Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default = "default_count")]
    count: u32,
    #[serde(default)]
    thread_ts: Option<String>,
}

const fn default_count() -> u32 {
    20
}

#[async_trait]
impl Tool for SlackSearchTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "slack_search searches Slack messages. Args: query, count, optional thread_ts context. Returns at most 20 results and truncates each result text to 300 characters."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {"type": "string"},
                "count": {"type": "integer", "minimum": 1, "maximum": 20, "default": 20},
                "thread_ts": {"type": "string"}
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: SearchArgs = parse_args(args)?;
        gate_tool(self.policy.as_ref(), ctx, TOOL_NAME).await?;
        let count = args.count.clamp(1, MAX_RESULTS_U32);
        let response = self
            .client
            .search_messages(&args.query, count)
            .await
            .map_err(|err| slack_error_to_tool_error(&err))?;
        let matches = response
            .messages
            .matches
            .into_iter()
            .take(MAX_RESULTS)
            .map(|item| {
                json!({
                    "text": item.text.map(|text| truncate_chars(&text, MAX_TEXT_CHARS)),
                    "username": item.username,
                    "ts": item.ts,
                    "permalink": item.permalink
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "matches": matches,
            "thread_ts": args.thread_ts
        }))
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        Ok(soft_result(self.execute(args, ctx).await))
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    text.chars().take(max_chars).collect()
}
