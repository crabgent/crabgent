use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::action::Action;
use crate::error::{KernelError, ToolError};
use crate::hook::RunCtx;
use crate::model::{ResolvedEffort, ResolvedModelWithSource};
use crate::policy::{PolicyDecision, PolicyHook};
use crate::provider::Provider;
use crate::tool::{Tool, ToolCtx};
use crate::types::{LlmRequest, ToolCall};

use super::{MAX_ADVERTISED_TOOLS, advertised_tools, resolve_tool_call};

struct StubTool;

#[async_trait]
impl Tool for StubTool {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn description(&self) -> &'static str {
        "test stub"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(json!({"ok": true}))
    }
}

struct PermissionTool;

#[async_trait]
impl Tool for PermissionTool {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn description(&self) -> &'static str {
        "test permission error"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Err(ToolError::Permission("inner deny".into()))
    }
}

struct ExecutionTool;

#[async_trait]
impl Tool for ExecutionTool {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn description(&self) -> &'static str {
        "test execution error"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Err(ToolError::Execution("boom".into()))
    }
}

struct NotFoundTool;

#[async_trait]
impl Tool for NotFoundTool {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn description(&self) -> &'static str {
        "test not found error"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Err(ToolError::NotFound("missing stub id".into()))
    }
}

struct AllowAllTestPolicy;

#[async_trait]
impl PolicyHook for AllowAllTestPolicy {
    async fn allow(&self, _subject: &crate::Subject, _action: &Action) -> PolicyDecision {
        PolicyDecision::Allow
    }
}

struct DenyToolCallPolicy;

#[async_trait]
impl PolicyHook for DenyToolCallPolicy {
    async fn allow(&self, _subject: &crate::Subject, action: &Action) -> PolicyDecision {
        match action {
            Action::ToolCall(name) if name == "stub" => PolicyDecision::Deny("stub denied".into()),
            _ => PolicyDecision::Allow,
        }
    }
}

struct LimitProvider(Option<usize>);

#[async_trait]
impl Provider for LimitProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<crate::types::LlmResponse, crate::error::ProviderError> {
        Err(crate::error::ProviderError::Other(
            "unused test provider".into(),
        ))
    }

    fn name(&self) -> &'static str {
        "limit-provider"
    }

    fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities::default()
    }

    fn tool_advertise_limit(&self) -> Option<usize> {
        self.0
    }
}

fn stub_tools(count: usize) -> Vec<Arc<dyn Tool>> {
    (0..count)
        .map(|_| Arc::new(StubTool) as Arc<dyn Tool>)
        .collect()
}

fn stub_call() -> ToolCall {
    ToolCall {
        id: "test-id".into(),
        name: "stub".into(),
        args: json!({}),
        thought_signature: None,
    }
}

fn test_ctx() -> RunCtx {
    RunCtx::new(crate::RunId::new(), crate::Subject::new("test-subject"))
}

fn current_model() -> ResolvedModelWithSource {
    ResolvedModelWithSource {
        info: crate::model::ModelInfo::minimal("test-model", "limit-provider"),
        source: crate::model::ResolvedSource::ConfigDefault,
    }
}

fn current_effort() -> ResolvedEffort {
    ResolvedEffort {
        effort: None,
        source: crate::model::EffortSource::ModelDefault,
    }
}

#[tokio::test]
async fn resolve_tool_call_soft_denies_outer_action_tool_call() {
    let tools = vec![Arc::new(StubTool) as Arc<dyn Tool>];
    let result = resolve_tool_call(
        &DenyToolCallPolicy,
        &tools,
        &stub_call(),
        &test_ctx(),
        &current_model(),
        &current_effort(),
        None,
    )
    .await
    .expect("outer tool-call denial is a soft error");

    assert!(result.is_error);
    assert_eq!(result.output, json!("stub denied"));
    assert_eq!(result.call_id, "test-id");
}

#[tokio::test]
async fn resolve_tool_call_soft_denies_inner_permission() {
    let tools = vec![Arc::new(PermissionTool) as Arc<dyn Tool>];
    let result = resolve_tool_call(
        &AllowAllTestPolicy,
        &tools,
        &stub_call(),
        &test_ctx(),
        &current_model(),
        &current_effort(),
        None,
    )
    .await
    .expect("inner permission denial is a soft error");

    assert!(result.is_error);
    assert_eq!(result.output, json!("inner deny"));
    assert_eq!(result.call_id, "test-id");
}

#[tokio::test]
async fn resolve_tool_call_maps_not_found_to_soft_error() {
    let tools = vec![Arc::new(NotFoundTool) as Arc<dyn Tool>];
    let result = resolve_tool_call(
        &AllowAllTestPolicy,
        &tools,
        &stub_call(),
        &test_ctx(),
        &current_model(),
        &current_effort(),
        None,
    )
    .await
    .expect("not found is a soft error");

    assert!(result.is_error);
    assert_eq!(result.output, json!("missing stub id"));
    assert_eq!(result.call_id, "test-id");
}

#[tokio::test]
async fn resolve_tool_call_propagates_hard_execution_error() {
    let tools = vec![Arc::new(ExecutionTool) as Arc<dyn Tool>];
    let err = resolve_tool_call(
        &AllowAllTestPolicy,
        &tools,
        &stub_call(),
        &test_ctx(),
        &current_model(),
        &current_effort(),
        None,
    )
    .await
    .expect_err("hard execution error aborts run");

    assert!(matches!(
        err,
        KernelError::Tool(ToolError::Execution(message)) if message == "boom"
    ));
}

#[test]
fn advertised_tools_allows_no_tools() {
    let provider = LimitProvider(None);
    let tools = stub_tools(0);
    let defs = advertised_tools(&provider, &tools).expect("empty tool set should advertise");
    assert!(defs.is_empty());
}

#[test]
fn advertised_tools_allows_default_limit() {
    let provider = LimitProvider(None);
    let tools = stub_tools(MAX_ADVERTISED_TOOLS);
    let defs = advertised_tools(&provider, &tools).expect("limit-sized tool set should advertise");
    assert_eq!(defs.len(), MAX_ADVERTISED_TOOLS);
}

#[test]
fn advertised_tools_rejects_above_default_limit() {
    let provider = LimitProvider(None);
    let tools = stub_tools(MAX_ADVERTISED_TOOLS + 1);
    let err = advertised_tools(&provider, &tools).expect_err("tool set above limit should fail");

    assert!(matches!(
        &err,
        KernelError::TooManyTools {
            count,
            max: MAX_ADVERTISED_TOOLS
        } if *count == MAX_ADVERTISED_TOOLS + 1
    ));
    assert_eq!(
        err.to_string(),
        format!(
            "tool count {} exceeds advertised limit {}",
            MAX_ADVERTISED_TOOLS + 1,
            MAX_ADVERTISED_TOOLS
        )
    );
}

#[test]
fn advertised_tools_rejects_above_provider_override() {
    let provider = LimitProvider(Some(2));
    let tools = stub_tools(3);
    let err = advertised_tools(&provider, &tools)
        .expect_err("tool set above provider override should fail");

    assert!(matches!(
        &err,
        KernelError::TooManyTools { count: 3, max: 2 }
    ));
    assert_eq!(err.to_string(), "tool count 3 exceeds advertised limit 2");
}
