//! `KernelBuilder` typestate tests.

use super::*;
use crate::error::ProviderError;
use crate::hook::RunCtx;
use crate::model::{ModelId, ModelInfo};
use crate::policy::AllowAllPolicy;
use crate::provider::ProviderCapabilities;
use crate::tool::ToolCtx;
use crate::types::{LlmRequest, LlmResponse};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Other(
            "stub provider complete is not used in builder tests".to_owned(),
        ))
    }
    fn name(&self) -> &'static str {
        "stub"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("stub", "stub")]
    }
}

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "stub"
    }
    fn parameters_schema(&self) -> Value {
        json!({})
    }
    async fn execute(
        &self,
        _args: Value,
        _ctx: &ToolCtx,
    ) -> Result<Value, crate::error::ToolError> {
        Ok(json!({}))
    }
}

#[test]
fn builder_new_starts_empty() {
    let b = Kernel::builder();
    let _ = b;
}

#[test]
fn builder_with_provider_and_policy_builds() {
    let kernel = Kernel::builder()
        .provider(StubProvider)
        .policy(AllowAllPolicy)
        .build();
    assert_eq!(kernel.provider_name(), "stub");
    assert_eq!(kernel.tool_count(), 0);
    assert_eq!(kernel.hook_count(), 0);
}

#[test]
fn add_tool_works_in_any_typestate() {
    let kernel = Kernel::builder()
        .add_tool(EchoTool)
        .provider(StubProvider)
        .add_tool(EchoTool)
        .policy(AllowAllPolicy)
        .add_tool(EchoTool)
        .build();
    assert_eq!(kernel.tool_count(), 3);
    assert!(kernel.tool("echo").is_some());
    assert!(kernel.tool("missing").is_none());
}

#[test]
fn defaults_override_persists() {
    let kernel = Kernel::builder()
        .provider(StubProvider)
        .policy(AllowAllPolicy)
        .defaults(Defaults { max_turns: 7 })
        .build();
    assert_eq!(kernel.defaults().max_turns, 7);
}

#[test]
fn defaults_default_is_50_turns() {
    let kernel = Kernel::builder()
        .provider(StubProvider)
        .policy(AllowAllPolicy)
        .build();
    assert_eq!(kernel.defaults().max_turns, Defaults::DEFAULT_MAX_TURNS);
}

#[test]
fn stream_buffer_size_default_is_64() {
    assert_eq!(Defaults::STREAM_BUFFER_SIZE, 64);
}

#[test]
fn provider_can_be_set_before_or_after_policy() {
    let k1 = Kernel::builder()
        .provider(StubProvider)
        .policy(AllowAllPolicy)
        .build();
    let k2 = Kernel::builder()
        .policy(AllowAllPolicy)
        .provider(StubProvider)
        .build();
    assert_eq!(k1.provider_name(), k2.provider_name());
}

#[test]
fn tool_lookup_returns_first_match() {
    let kernel = Kernel::builder()
        .provider(StubProvider)
        .policy(AllowAllPolicy)
        .add_tool(EchoTool)
        .build();
    let t = kernel.tool("echo").expect("echo registered");
    assert_eq!(t.name(), "echo");
}

struct ModelStubProvider {
    models: Vec<ModelInfo>,
}

#[async_trait]
impl Provider for ModelStubProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Other(
            "model stub provider complete is not used in builder tests".to_owned(),
        ))
    }
    fn name(&self) -> &'static str {
        "model-stub"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
    fn models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }
}

#[test]
fn build_populates_registry_from_provider_models() {
    let provider = ModelStubProvider {
        models: vec![
            ModelInfo::minimal("m1", "model-stub"),
            ModelInfo::minimal("m2", "model-stub"),
        ],
    };
    let kernel = Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .build();
    assert_eq!(kernel.models().len(), 2);
    assert!(kernel.models().get(&ModelId::new("m1")).is_some());
    assert!(kernel.models().get(&ModelId::new("m2")).is_some());
}

#[test]
#[should_panic(expected = "duplicate model id: dup")]
fn build_panics_on_provider_duplicate_models() {
    let provider = ModelStubProvider {
        models: vec![
            ModelInfo::minimal("dup", "model-stub"),
            ModelInfo::minimal("dup", "model-stub"),
        ],
    };
    let _kernel = Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .build();
}

#[test]
fn build_with_stub_provider_registers_minimal_model() {
    let kernel = Kernel::builder()
        .provider(StubProvider)
        .policy(AllowAllPolicy)
        .build();
    assert_eq!(kernel.models().len(), 1);
}
