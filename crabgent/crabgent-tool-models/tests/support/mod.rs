#![allow(
    dead_code,
    reason = "integration test helper module is compiled separately by each test binary"
)]

use std::collections::HashMap;
use std::sync::Arc;

use crabgent_core::{
    AllowAllPolicy, DenyAllPolicy, EffortSource, GlobalModelOverrideStore,
    GlobalReasoningEffortOverrideStore, Kernel, KernelBuilder, MemoryScope, ModelCapabilities,
    ModelId, ModelInfo, PolicyHook, Pricing, ResolvedEffort, ResolvedModelWithSource,
    ResolvedSource, Subject, Tool, ToolCtx, ToolError,
};
use crabgent_store::memory::{MemoryGlobalOverrideStore, MemorySessionStore};
use crabgent_store::{Owner, SessionId, SessionStore};
use crabgent_test_support::StubProvider;
use crabgent_tool_models::ModelRegistryTool;
use serde_json::Value;

fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("alice"))
}

pub fn allow_policy() -> Arc<dyn PolicyHook> {
    Arc::new(AllowAllPolicy)
}

pub fn deny_policy() -> Arc<dyn PolicyHook> {
    Arc::new(DenyAllPolicy)
}

const fn caps() -> ModelCapabilities {
    ModelCapabilities {
        max_input_tokens: 200_000,
        max_output_tokens: 8_192,
        default_max_output_tokens: 4_096,
        default_temperature_milli: 1_000,
        supports_tools: true,
        supports_vision: false,
        supports_audio: false,
        supports_thinking: true,
        supports_prompt_cache: true,
        supports_web_search: false,
        supports_temperature: true,
        reasoning_effort: None,
    }
}

const fn pricing() -> Pricing {
    Pricing {
        input_per_mtok_usd: 3.0,
        output_per_mtok_usd: 15.0,
        cache_read_per_mtok_usd: Some(0.3),
        cache_write_per_mtok_usd: Some(3.75),
    }
}

fn model(
    provider: &'static str,
    id: impl Into<String>,
    display_name: impl Into<String>,
    aliases: Vec<ModelId>,
    pricing: Option<Pricing>,
) -> ModelInfo {
    ModelInfo {
        id: ModelId::new(id),
        provider: provider.to_owned(),
        display_name: display_name.into(),
        aliases,
        caps: caps(),
        pricing,
        extensions: HashMap::new(),
    }
}

fn provider(name: &'static str, models: Vec<ModelInfo>) -> StubProvider {
    StubProvider::new().with_name(name).with_models(models)
}

fn fixture_kernel() -> Arc<Kernel> {
    Arc::new(
        KernelBuilder::new()
            .provider(provider(
                "anthropic",
                vec![
                    model(
                        "anthropic",
                        "claude-sonnet-4-6",
                        "Claude Sonnet 4.6",
                        vec![ModelId::new("sonnet")],
                        Some(pricing()),
                    ),
                    model(
                        "anthropic",
                        "claude-haiku-4-5",
                        "Claude Haiku 4.5",
                        Vec::new(),
                        Some(pricing()),
                    ),
                ],
            ))
            .provider(provider(
                "openai",
                vec![model("openai", "gpt-5.5", "GPT-5.5", Vec::new(), None)],
            ))
            .policy(AllowAllPolicy)
            .build(),
    )
}

pub fn bulk_kernel(count: usize) -> Arc<Kernel> {
    let models = (0..count)
        .map(|idx| {
            model(
                "bulk",
                format!("bulk-{idx:03}"),
                "Bulk Model",
                Vec::new(),
                None,
            )
        })
        .collect();
    Arc::new(
        KernelBuilder::new()
            .provider(provider("bulk", models))
            .policy(AllowAllPolicy)
            .build(),
    )
}

pub fn tool(policy: Arc<dyn PolicyHook>) -> ModelRegistryTool {
    let (tool, _, _) = tool_with_state(policy);
    tool
}

pub fn tool_for_kernel(kernel: Arc<Kernel>, policy: Arc<dyn PolicyHook>) -> ModelRegistryTool {
    let store: Arc<MemorySessionStore> = Arc::new(MemorySessionStore::default());
    let global_store: Arc<MemoryGlobalOverrideStore> =
        Arc::new(MemoryGlobalOverrideStore::default());
    let store_dyn: Arc<dyn SessionStore> = store;
    let global_dyn: Arc<dyn GlobalModelOverrideStore> = global_store.clone();
    let effort_dyn: Arc<dyn GlobalReasoningEffortOverrideStore> = global_store;
    ModelRegistryTool::new(kernel, policy, store_dyn, global_dyn, effort_dyn)
}

pub fn tool_with_state(
    policy: Arc<dyn PolicyHook>,
) -> (
    ModelRegistryTool,
    Arc<MemorySessionStore>,
    Arc<MemoryGlobalOverrideStore>,
) {
    let store: Arc<MemorySessionStore> = Arc::new(MemorySessionStore::default());
    let global_store: Arc<MemoryGlobalOverrideStore> =
        Arc::new(MemoryGlobalOverrideStore::default());
    let store_dyn: Arc<dyn SessionStore> = store.clone();
    let global_dyn: Arc<dyn GlobalModelOverrideStore> = global_store.clone();
    let effort_dyn: Arc<dyn GlobalReasoningEffortOverrideStore> = global_store.clone();
    (
        ModelRegistryTool::new(fixture_kernel(), policy, store_dyn, global_dyn, effort_dyn),
        store,
        global_store,
    )
}

pub async fn execute(tool: &ModelRegistryTool, args: Value) -> Result<Value, ToolError> {
    tool.execute(args, &ctx()).await
}

pub async fn execute_with_ctx(
    tool: &ModelRegistryTool,
    args: Value,
    ctx: &ToolCtx,
) -> Result<Value, ToolError> {
    tool.execute(args, ctx).await
}

pub async fn save_session(store: &MemorySessionStore, model_override: Option<&str>) -> SessionId {
    save_session_for(store, "alice", model_override).await
}

pub async fn save_session_for(
    store: &MemorySessionStore,
    owner: &str,
    model_override: Option<&str>,
) -> SessionId {
    let mut session = store
        .find_or_create(&Owner::new(owner), None, &MemoryScope::default())
        .await
        .expect("test result");
    session.model_override = model_override.map(str::to_owned);
    let id = session.id.clone();
    store.save(&session).await.expect("test result");
    id
}

pub fn ctx_with_current(model_id: &str, source: ResolvedSource) -> ToolCtx {
    ToolCtx::new(Subject::new("alice"))
        .with_current_model(ResolvedModelWithSource {
            info: model(
                "anthropic",
                model_id,
                "Current Model",
                Vec::new(),
                Some(pricing()),
            ),
            source,
        })
        .with_current_effort(ResolvedEffort {
            effort: None,
            source: EffortSource::ModelDefault,
        })
}

pub fn ids_from(result: &Value) -> Vec<String> {
    result["models"]
        .as_array()
        .expect("models array")
        .iter()
        .map(|model| model["id"].as_str().expect("model id").to_owned())
        .collect()
}
