//! Per-run `reasoning_effort` override propagation tests.
//!
//! Cover four cases:
//!   1. Override-set: caller's `RunRequest.reasoning_effort` reaches the provider.
//!   2. Override-none: provider receives the model-capability default.
//!   3. Override-wins: caller override beats a non-`None` model-capability default.
//!   4. Multi-attempt fallback: override survives supported provider fallback.
//!   5. Unsupported fallback: override downgrades to a plain request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, Event, GlobalReasoningEffortOverrideStore, Kernel, KernelError,
    LlmRequest, LlmResponse, Message, ModelCapabilities, ModelId, ModelInfo, ModelTarget, Provider,
    ProviderCapabilities, ProviderError, ReasoningEffort, ReasoningEffortOverrideStoreError,
    RunCtx, RunId, RunRequest, StopReason, Subject, Usage,
};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

struct CapturingProvider {
    name: &'static str,
    model_info: ModelInfo,
    fail_first: bool,
    calls: Arc<Mutex<u32>>,
    seen: Arc<Mutex<Vec<Option<ReasoningEffort>>>>,
}

struct StaticEffortStore(Option<ReasoningEffort>);

#[async_trait]
impl GlobalReasoningEffortOverrideStore for StaticEffortStore {
    async fn get_global_reasoning_effort_override(
        &self,
    ) -> Result<Option<ReasoningEffort>, ReasoningEffortOverrideStoreError> {
        Ok(self.0)
    }

    async fn set_global_reasoning_effort_override(
        &self,
        _effort: ReasoningEffort,
    ) -> Result<(), ReasoningEffortOverrideStoreError> {
        Err(ReasoningEffortOverrideStoreError::backend(
            "test store is read-only",
        ))
    }

    async fn clear_global_reasoning_effort_override(
        &self,
    ) -> Result<(), ReasoningEffortOverrideStoreError> {
        Err(ReasoningEffortOverrideStoreError::backend(
            "test store is read-only",
        ))
    }
}

impl CapturingProvider {
    fn new(
        name: &'static str,
        model_info: ModelInfo,
    ) -> (Self, Arc<Mutex<Vec<Option<ReasoningEffort>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                name,
                model_info,
                fail_first: false,
                calls: Arc::new(Mutex::new(0)),
                seen: Arc::clone(&seen),
            },
            seen,
        )
    }
}

#[async_trait]
impl Provider for CapturingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.seen
            .lock()
            .expect("seen mutex not poisoned")
            .push(req.reasoning_effort);
        let mut calls = self.calls.lock().expect("calls mutex not poisoned");
        *calls += 1;
        if self.fail_first && *calls == 1 {
            return Err(ProviderError::Other(
                "scripted first-attempt failure".into(),
            ));
        }
        Ok(LlmResponse {
            text: "done".to_owned(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![self.model_info.clone()]
    }
}

const fn caps_with_effort(effort: Option<ReasoningEffort>) -> ModelCapabilities {
    ModelCapabilities {
        max_input_tokens: 200_000,
        max_output_tokens: 4_096,
        default_max_output_tokens: 4_096,
        default_temperature_milli: 1_000,
        supports_tools: true,
        supports_vision: false,
        supports_audio: false,
        supports_thinking: false,
        supports_prompt_cache: false,
        reasoning_effort: effort,
        supports_web_search: false,
        supports_temperature: true,
    }
}

fn model_info(id: &str, provider: &'static str, effort: Option<ReasoningEffort>) -> ModelInfo {
    ModelInfo {
        id: ModelId::new(id),
        provider: provider.into(),
        display_name: id.into(),
        aliases: Vec::new(),
        caps: caps_with_effort(effort),
        pricing: None,
        extensions: HashMap::new(),
    }
}

fn run_request(model: &str, override_effort: Option<ReasoningEffort>) -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("reasoning-effort-user"),
        model: ModelTarget::id(ModelId::new(model)),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text { text: "hi".into() }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(1),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: override_effort,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

async fn drive_to_final(kernel: &Kernel, req: RunRequest) {
    let stream = kernel.run_streaming(req, None);
    tokio::pin!(stream);
    while let Some(item) = stream.next().await {
        if let Event::Final(_) = item.expect("stream item is Ok") {
            break;
        }
    }
}

#[tokio::test]
async fn run_request_propagates_reasoning_effort_override() {
    let info = model_info("rmodel", "rprov", Some(ReasoningEffort::Low));
    let (provider, seen) = CapturingProvider::new("rprov", info);
    let kernel = Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .build();
    drive_to_final(&kernel, run_request("rmodel", Some(ReasoningEffort::High))).await;

    let observed = seen.lock().expect("seen mutex not poisoned");
    assert_eq!(observed.as_slice(), &[Some(ReasoningEffort::High)]);
}

#[tokio::test]
async fn run_request_none_falls_back_to_model_capability() {
    let info = model_info("rmodel", "rprov", Some(ReasoningEffort::Low));
    let (provider, seen) = CapturingProvider::new("rprov", info);
    let kernel = Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .build();
    drive_to_final(&kernel, run_request("rmodel", None)).await;

    let observed = seen.lock().expect("seen mutex not poisoned");
    assert_eq!(observed.as_slice(), &[Some(ReasoningEffort::Low)]);
}

#[tokio::test]
async fn run_request_override_wins_over_model_capability() {
    let info = model_info("rmodel", "rprov", Some(ReasoningEffort::Low));
    let (provider, seen) = CapturingProvider::new("rprov", info);
    let kernel = Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .build();
    drive_to_final(
        &kernel,
        run_request("rmodel", Some(ReasoningEffort::Medium)),
    )
    .await;

    let observed = seen.lock().expect("seen mutex not poisoned");
    assert_eq!(observed.as_slice(), &[Some(ReasoningEffort::Medium)]);
}

#[tokio::test]
async fn multi_attempt_fallback_preserves_override() {
    let primary_info = model_info("primary", "primary-prov", Some(ReasoningEffort::Low));
    let fallback_info = model_info("fallback", "fallback-prov", Some(ReasoningEffort::Low));
    let (mut primary, primary_seen) = CapturingProvider::new("primary-prov", primary_info);
    primary.fail_first = true;
    let (fallback, fallback_seen) = CapturingProvider::new("fallback-prov", fallback_info);

    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .build();

    let mut req = run_request("primary", Some(ReasoningEffort::High));
    req.fallbacks = vec![ModelTarget::id(ModelId::new("fallback"))];
    drive_to_final(&kernel, req).await;

    let primary_obs = primary_seen.lock().expect("primary mutex not poisoned");
    let fallback_obs = fallback_seen.lock().expect("fallback mutex not poisoned");
    assert_eq!(primary_obs.as_slice(), &[Some(ReasoningEffort::High)]);
    assert_eq!(fallback_obs.as_slice(), &[Some(ReasoningEffort::High)]);
}

#[tokio::test]
async fn multi_attempt_fallback_clears_override_when_caps_lack_support() {
    let primary_info = model_info("primary", "primary-prov", Some(ReasoningEffort::Low));
    let fallback_info = model_info("fallback", "fallback-prov", None);
    let (mut primary, primary_seen) = CapturingProvider::new("primary-prov", primary_info);
    primary.fail_first = true;
    let (fallback, fallback_seen) = CapturingProvider::new("fallback-prov", fallback_info);

    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .build();

    let mut req = run_request("primary", Some(ReasoningEffort::High));
    req.fallbacks = vec![ModelTarget::id(ModelId::new("fallback"))];
    drive_to_final(&kernel, req).await;

    let primary_obs = primary_seen.lock().expect("primary mutex not poisoned");
    let fallback_obs = fallback_seen.lock().expect("fallback mutex not poisoned");
    assert_eq!(primary_obs.as_slice(), &[Some(ReasoningEffort::High)]);
    assert_eq!(fallback_obs.as_slice(), &[None]);
}

#[tokio::test]
async fn global_effort_override_wins_over_model_default() {
    let info = model_info("rmodel", "rprov", Some(ReasoningEffort::Low));
    let (provider, seen) = CapturingProvider::new("rprov", info);
    let kernel = Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .with_global_reasoning_effort_override_store(Arc::new(StaticEffortStore(Some(
            ReasoningEffort::High,
        ))))
        .build();
    drive_to_final(&kernel, run_request("rmodel", None)).await;

    let observed = seen.lock().expect("seen mutex not poisoned");
    assert_eq!(observed.as_slice(), &[Some(ReasoningEffort::High)]);
}

#[tokio::test]
async fn forced_effort_on_unsupported_model_fails_before_provider() {
    let info = model_info("rmodel", "rprov", None);
    let (provider, seen) = CapturingProvider::new("rprov", info);
    let kernel = Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .with_global_reasoning_effort_override_store(Arc::new(StaticEffortStore(Some(
            ReasoningEffort::High,
        ))))
        .build();

    let err = kernel
        .run(run_request("rmodel", None), None)
        .await
        .expect_err("unsupported forced effort fails");

    assert!(matches!(
        err,
        KernelError::Provider(ProviderError::ReasoningEffortUnsupported { .. })
    ));
    let observed = seen.lock().expect("seen mutex not poisoned");
    assert!(observed.is_empty());
}
