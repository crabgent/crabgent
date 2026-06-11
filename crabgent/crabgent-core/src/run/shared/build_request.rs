//! Shared run-loop helpers used by sync and streaming drivers.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::action::Action;
use crate::error::{KernelError, ToolError};
use crate::hook::RunCtx;
use crate::model::{ModelId, ReasoningEffort, ResolvedEffort, ResolvedModelWithSource};
use crate::policy::{PolicyDecision, PolicyHook};
use crate::provider::Provider;
use crate::tool::{Tool, ToolCtx};
use crate::types::{LlmRequest, ToolCall, ToolDef, ToolResult, WebSearchConfig};

/// Default advertised tool cap for providers that do not report their own.
pub const MAX_ADVERTISED_TOOLS: usize = 64;

pub fn check_cancel(cancel: Option<&CancellationToken>) -> Result<(), KernelError> {
    match cancel {
        Some(t) if t.is_cancelled() => Err(KernelError::Cancelled),
        _ => Ok(()),
    }
}

/// Cooperative pause check. Unlike cancellation, the pause token is never
/// raced in a `select!`; it is only polled at safe boundaries (turn start
/// and between tool dispatches), so an in-flight provider stream or tool
/// future is never interrupted by a pause request.
pub fn check_pause(pause: &CancellationToken) -> Result<(), KernelError> {
    if pause.is_cancelled() {
        return Err(KernelError::Paused);
    }
    Ok(())
}

pub async fn check_policy(
    policy: &dyn PolicyHook,
    ctx: &RunCtx,
    action: &Action,
) -> Result<(), KernelError> {
    match policy.allow(&ctx.subject, action).await {
        PolicyDecision::Allow => Ok(()),
        PolicyDecision::Deny(reason) => Err(KernelError::PolicyDenied { reason }),
    }
}

pub fn build_request(
    model: &ModelId,
    system_prompt: Option<&str>,
    messages: &[Value],
    tools: &[ToolDef],
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    reasoning_effort: Option<ReasoningEffort>,
) -> LlmRequest {
    LlmRequest {
        model: model.clone(),
        system_prompt: system_prompt.map(str::to_string),
        messages: messages.to_vec(),
        tools: tools.to_vec(),
        max_tokens,
        temperature,
        stop_sequences: vec![],
        reasoning_effort,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    }
}

#[cfg(test)]
pub fn advertised_tools(
    provider: &dyn Provider,
    tools: &[Arc<dyn Tool>],
) -> Result<Vec<ToolDef>, KernelError> {
    check_tool_advertise_limit(provider, tools.len())?;
    Ok(tool_defs(tools))
}

pub fn tool_defs(tools: &[Arc<dyn Tool>]) -> Vec<ToolDef> {
    tools
        .iter()
        .map(|t| ToolDef {
            name: t.name().to_string(),
            description: t.description().to_string(),
            input_schema: t.parameters_schema(),
        })
        .collect()
}

pub fn check_tool_advertise_limit(
    provider: &dyn Provider,
    count: usize,
) -> Result<(), KernelError> {
    let max_tools = provider
        .tool_advertise_limit()
        .unwrap_or(MAX_ADVERTISED_TOOLS);
    if count > max_tools {
        return Err(KernelError::TooManyTools {
            count,
            max: max_tools,
        });
    }

    Ok(())
}

pub async fn dispatch_tool(
    tools: &[Arc<dyn Tool>],
    call: &ToolCall,
    ctx: &RunCtx,
    current_model: &ResolvedModelWithSource,
    current_effort: &ResolvedEffort,
    cancel: Option<&CancellationToken>,
) -> Result<ToolResult, KernelError> {
    let tool = tools
        .iter()
        .find(|t| t.name() == call.name)
        .ok_or_else(|| KernelError::Tool(ToolError::NotFound(call.name.clone())))?;
    let mut tool_ctx = ToolCtx::new(ctx.subject.clone())
        .with_current_model(current_model.clone())
        .with_current_effort(*current_effort);
    if let Some(token) = cancel {
        tool_ctx = tool_ctx.with_cancel(token.clone());
    }
    if let Some(session_id) = ctx.session_id() {
        tool_ctx = tool_ctx.with_session_id(session_id);
    }
    let result = tool.execute_result(call.args.clone(), &tool_ctx).await?;
    Ok(result.with_call_id(call.id.clone()))
}

pub async fn resolve_tool_call(
    policy: &dyn PolicyHook,
    tools: &[Arc<dyn Tool>],
    call: &ToolCall,
    ctx: &RunCtx,
    current_model: &ResolvedModelWithSource,
    current_effort: &ResolvedEffort,
    cancel: Option<&CancellationToken>,
) -> Result<ToolResult, KernelError> {
    match policy
        .allow(&ctx.subject, &Action::tool(call.name.clone()))
        .await
    {
        PolicyDecision::Deny(reason) => {
            return Ok(ToolResult::soft_error(json!(reason)).with_call_id(call.id.clone()));
        }
        PolicyDecision::Allow => {}
    }
    match dispatch_tool(tools, call, ctx, current_model, current_effort, cancel).await {
        Ok(result) => Ok(result),
        // SAFETY/INVARIANT: this normalization is the canonical cancel-mapping
        // point; stream.rs::outcome_for_error matches on the normalized variant
        // only.
        Err(KernelError::Tool(ToolError::Cancelled)) => Err(KernelError::Cancelled),
        Err(KernelError::Tool(
            ToolError::Permission(reason)
            | ToolError::InvalidArgs(reason)
            | ToolError::NotFound(reason),
        )) => Ok(ToolResult::soft_error(json!(reason)).with_call_id(call.id.clone())),
        Err(other) => Err(other),
    }
}
