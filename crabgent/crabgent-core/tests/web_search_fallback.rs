//! Web-search request shaping across provider fallback attempts.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, Decision, EventStream, Hook, Kernel, KernelError, LlmRequest,
    LlmResponse, Message, ModelInfo, ModelTarget, Provider, ProviderCapabilities, ProviderError,
    ProviderEvent, RunCtx, RunId, RunRequest, StopReason, Subject, Usage, WebSearchConfig,
};
use futures::stream;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
enum PrimaryFailure {
    Open,
    Pump,
}

struct FailingPrimaryProvider {
    failure: PrimaryFailure,
    calls: Arc<AtomicUsize>,
    supports_web_search: bool,
}

struct CapturingFallbackProvider {
    seen: Arc<Mutex<Vec<WebSearchConfig>>>,
}

struct AfterLlmRequestHook {
    seen: Arc<Mutex<Vec<WebSearchConfig>>>,
}

struct ObservedWebSearch {
    provider: Vec<WebSearchConfig>,
    after_llm: Vec<WebSearchConfig>,
}

#[async_trait]
impl Provider for FailingPrimaryProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Other("stream-only primary provider".into()))
    }

    async fn stream(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<EventStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.failure {
            PrimaryFailure::Open => Err(ProviderError::Api {
                status: 503,
                message: "primary unavailable".into(),
                retry_after_secs: None,
            }),
            PrimaryFailure::Pump => {
                let events: Vec<Result<ProviderEvent, ProviderError>> =
                    vec![Err(ProviderError::RetryableStream {
                        message: "primary stream reset".into(),
                    })];
                Ok(Box::pin(stream::iter(events)))
            }
        }
    }

    fn name(&self) -> &'static str {
        "primary-web"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_caps()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![model_info(
            "primary-model",
            self.name(),
            self.supports_web_search,
        )]
    }
}

#[async_trait]
impl Provider for CapturingFallbackProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.seen
            .lock()
            .expect("seen mutex not poisoned")
            .push(req.web_search.clone());
        Ok(LlmResponse {
            text: "fallback done".into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "fallback-web"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_caps()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![model_info("fallback-model", self.name(), false)]
    }
}

#[async_trait]
impl Hook for AfterLlmRequestHook {
    async fn after_llm(
        &self,
        req: &LlmRequest,
        _resp: &LlmResponse,
        _ctx: &RunCtx,
    ) -> Decision<LlmResponse> {
        self.seen
            .lock()
            .expect("seen mutex not poisoned")
            .push(req.web_search.clone());
        Decision::Continue
    }
}

fn provider_caps() -> ProviderCapabilities {
    ProviderCapabilities {
        web_search: true,
        ..ProviderCapabilities::default()
    }
}

fn model_info(id: &str, provider: &str, supports_web_search: bool) -> ModelInfo {
    let mut info = ModelInfo::minimal(id, provider.to_owned());
    info.caps.supports_web_search = supports_web_search;
    info
}

fn web_search_config() -> WebSearchConfig {
    WebSearchConfig {
        enabled: true,
        max_uses: Some(2),
        allowed_domains: vec!["example.org".into()],
        blocked_domains: Vec::new(),
    }
}

fn run_request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("web-search-user"),
        model: ModelTarget::id("primary-model"),
        explicit_model: None,
        session_model_override: None,
        fallbacks: vec![ModelTarget::new("fallback-web", "fallback-model")],
        messages: vec![Message::User {
            content: vec![ContentBlock::Text { text: "hi".into() }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(1),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: web_search_config(),
    }
}

async fn run_with_primary_failure(failure: PrimaryFailure) -> ObservedWebSearch {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let fallback_seen = Arc::new(Mutex::new(Vec::new()));
    let after_llm_seen = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(FailingPrimaryProvider {
            failure,
            calls: Arc::clone(&primary_calls),
            supports_web_search: true,
        })
        .provider(CapturingFallbackProvider {
            seen: Arc::clone(&fallback_seen),
        })
        .add_hook(AfterLlmRequestHook {
            seen: Arc::clone(&after_llm_seen),
        })
        .policy(AllowAllPolicy)
        .build();

    let text = kernel.run(run_request(), None).await.expect("test result");

    assert_eq!(text, "fallback done");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    ObservedWebSearch {
        provider: fallback_seen
            .lock()
            .expect("fallback seen mutex not poisoned")
            .clone(),
        after_llm: after_llm_seen
            .lock()
            .expect("after_llm seen mutex not poisoned")
            .clone(),
    }
}

#[tokio::test]
async fn open_fallback_downgrades_web_search_for_unsupported_fallback() {
    let seen = run_with_primary_failure(PrimaryFailure::Open).await;

    assert_eq!(seen.provider.as_slice(), &[WebSearchConfig::default()]);
    assert_eq!(seen.after_llm.as_slice(), &[WebSearchConfig::default()]);
}

#[tokio::test]
async fn pump_fallback_downgrades_web_search_for_unsupported_fallback() {
    let seen = run_with_primary_failure(PrimaryFailure::Pump).await;

    assert_eq!(seen.provider.as_slice(), &[WebSearchConfig::default()]);
    assert_eq!(seen.after_llm.as_slice(), &[WebSearchConfig::default()]);
}

#[tokio::test]
async fn primary_without_web_search_support_fails_closed_before_fallback() {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let fallback_seen = Arc::new(Mutex::new(Vec::new()));
    let after_llm_seen = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(FailingPrimaryProvider {
            failure: PrimaryFailure::Open,
            calls: Arc::clone(&primary_calls),
            supports_web_search: false,
        })
        .provider(CapturingFallbackProvider {
            seen: Arc::clone(&fallback_seen),
        })
        .add_hook(AfterLlmRequestHook {
            seen: Arc::clone(&after_llm_seen),
        })
        .policy(AllowAllPolicy)
        .build();

    let err = kernel
        .run(run_request(), None)
        .await
        .expect_err("expected fail-closed web-search error");

    assert!(matches!(
        err,
        KernelError::Provider(ProviderError::WebSearchUnsupported { provider, model })
            if provider == "primary-web" && model == "primary-model"
    ));
    assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
    assert!(
        fallback_seen
            .lock()
            .expect("fallback seen mutex not poisoned")
            .is_empty()
    );
    assert!(
        after_llm_seen
            .lock()
            .expect("after_llm seen mutex not poisoned")
            .is_empty()
    );
}
