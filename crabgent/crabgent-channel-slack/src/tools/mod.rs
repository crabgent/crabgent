//! Slack-specific tools advertised to LLM providers.

use std::sync::Arc;

use crabgent_core::action::Action;
use crabgent_core::error::ToolError;
use crabgent_core::policy::{PolicyDecision, PolicyHook};
use crabgent_core::tool::{Tool, ToolCtx};
use crabgent_core::types::ToolResult;
use serde_json::{Value, json};

use crate::api::SlackHttpClient;
use crate::error::SlackError;

mod search;

pub use search::SlackSearchTool;

/// Register Slack-specific tools with the same client and policy hook.
/// Generic channel operations live in `crabgent-channel` as `channel_*`
/// tools; Slack keeps only Slack-specific search here.
#[must_use]
pub fn register_slack_tools(
    client: Arc<SlackHttpClient>,
    policy: Arc<dyn PolicyHook>,
) -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(SlackSearchTool::new(client, policy))]
}

// Intentional local copy: Slack currently has one channel-specific tool, so
// exposing shared helper internals from `crabgent-channel` would widen the
// crate boundary without reducing real duplication.
async fn gate_tool(
    policy: &dyn PolicyHook,
    ctx: &ToolCtx,
    tool_name: &'static str,
) -> Result<(), ToolError> {
    match policy.allow(&ctx.subject, &Action::tool(tool_name)).await {
        PolicyDecision::Allow => Ok(()),
        PolicyDecision::Deny(reason) => Err(ToolError::Permission(reason)),
    }
}

fn soft_result(result: Result<Value, ToolError>) -> ToolResult {
    match result {
        Ok(value) => ToolResult::success(value),
        Err(ToolError::Permission(reason)) => ToolResult::soft_error(json!(reason)),
        Err(error) => ToolResult::soft_error(json!(error.to_string())),
    }
}

fn slack_error_to_tool_error(error: &SlackError) -> ToolError {
    match error {
        SlackError::Auth => ToolError::Permission("Slack authentication failed".to_owned()),
        SlackError::Membership => ToolError::Permission("Slack membership denied".to_owned()),
        SlackError::RateLimited { .. } => ToolError::Execution("Slack rate limited".to_owned()),
        SlackError::ApiError { .. } => ToolError::Execution("Slack API error".to_owned()),
        SlackError::InvalidToken => ToolError::Permission("Slack token is invalid".to_owned()),
        SlackError::Transport(_) | SlackError::Serde(_) | SlackError::Internal(_) => {
            ToolError::Execution("Slack request failed".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_tool_error_surface_is_opaque_for_api_failures() {
        let err = slack_error_to_tool_error(&SlackError::ApiError {
            slack_code: "team_disabled".to_owned(),
            http_status: Some(403),
        });

        assert!(matches!(err, ToolError::Execution(msg) if msg == "Slack API error"));
    }
}
