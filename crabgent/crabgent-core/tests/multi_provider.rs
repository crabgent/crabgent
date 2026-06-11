//! Multi-provider routing tests for the public kernel API.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, Decision, Event, Hook, Kernel, KernelError, LlmRequest,
    LlmResponse, Message, ModelId, ModelInfo, ModelTarget, Provider, ProviderCapabilities,
    ProviderError, RunCtx, RunId, RunRequest, StopReason, Subject, Usage,
};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

struct StaticProvider {
    name: &'static str,
    model: &'static str,
    text: &'static str,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for StaticProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            text: self.text.to_owned(),
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
        vec![ModelInfo::minimal(self.model, self.name)]
    }
}

struct FailingProvider {
    name: &'static str,
    model: &'static str,
    err: std::sync::Mutex<Option<ProviderError>>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for FailingProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(self
            .err
            .lock()
            .expect("test result")
            .take()
            .unwrap_or_else(|| ProviderError::Other("already failed once".into())))
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal(self.model, self.name)]
    }
}

enum ProviderStep {
    Fail(ProviderError),
    Text(&'static str),
}

struct CatalogProvider {
    name: &'static str,
    models: Vec<&'static str>,
    steps: std::sync::Mutex<Vec<ProviderStep>>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for CatalogProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut steps = self.steps.lock().expect("mutex should not be poisoned");
        if steps.is_empty() {
            return Err(ProviderError::Other("script exhausted".into()));
        }
        match steps.remove(0) {
            ProviderStep::Fail(err) => Err(err),
            ProviderStep::Text(text) => Ok(LlmResponse {
                text: text.to_owned(),
                tool_calls: Vec::new(),
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: req.model.clone(),
            }),
        }
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.models
            .iter()
            .map(|model| ModelInfo::minimal(*model, self.name))
            .collect()
    }
}

struct SwitchModelHook {
    target: ModelId,
}

#[async_trait]
impl Hook for SwitchModelHook {
    async fn before_llm(&self, req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        let mut next = req.clone();
        next.model = self.target.clone();
        Decision::Replace(next)
    }
}

fn provider(
    name: &'static str,
    model: &'static str,
    text: &'static str,
) -> (StaticProvider, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        StaticProvider {
            name,
            model,
            text,
            calls: Arc::clone(&calls),
        },
        calls,
    )
}

fn failing_provider(
    name: &'static str,
    model: &'static str,
    err: ProviderError,
) -> (FailingProvider, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        FailingProvider {
            name,
            model,
            err: std::sync::Mutex::new(Some(err)),
            calls: Arc::clone(&calls),
        },
        calls,
    )
}

fn catalog_provider(
    name: &'static str,
    models: Vec<&'static str>,
    steps: Vec<ProviderStep>,
) -> (CatalogProvider, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        CatalogProvider {
            name,
            models,
            steps: std::sync::Mutex::new(steps),
            calls: Arc::clone(&calls),
        },
        calls,
    )
}

fn request(model: &str) -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("user"),
        model: ModelTarget::id(model),
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
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

fn server_error() -> ProviderError {
    ProviderError::Api {
        status: 503,
        message: "upstream unavailable".into(),
        retry_after_secs: None,
    }
}

#[tokio::test]
async fn run_routes_to_provider_for_selected_model() {
    let (primary, primary_calls) = provider("primary", "primary-model", "wrong");
    let (fallback, fallback_calls) = provider("fallback", "fallback-model", "from fallback");
    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .build();

    let text = kernel
        .run(request("fallback-model"), None)
        .await
        .expect("test result");

    assert_eq!(text, "from fallback");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn before_llm_hook_can_switch_provider_by_switching_model() {
    let (primary, primary_calls) = provider("primary", "primary-model", "wrong");
    let (fallback, fallback_calls) = provider("fallback", "fallback-model", "from fallback");
    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .add_hook(SwitchModelHook {
            target: ModelId::new("fallback-model"),
        })
        .build();

    let text = kernel
        .run(request("primary-model"), None)
        .await
        .expect("test result");

    assert_eq!(text, "from fallback");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn run_request_with_model_target_id_only_resolves_unique_model() {
    let (primary, primary_calls) = provider("primary", "primary-model", "from primary");
    let (fallback, fallback_calls) = provider("fallback", "fallback-model", "wrong");
    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .build();
    let mut req = request("primary-model");
    req.model = ModelTarget::id("primary-model");

    let text = kernel.run(req, None).await.expect("test result");

    assert_eq!(text, "from primary");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn run_request_with_provider_qualified_model_target_routes_to_provider() {
    let (primary, primary_calls) = provider("primary", "primary-model", "wrong");
    let (fallback, fallback_calls) = provider("fallback", "fallback-model", "from fallback");
    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .build();
    let mut req = request("primary-model");
    req.model = ModelTarget::new("fallback", "fallback-model");

    let text = kernel.run(req, None).await.expect("test result");

    assert_eq!(text, "from fallback");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn streaming_routes_to_provider_for_selected_model() {
    let (primary, primary_calls) = provider("primary", "primary-model", "wrong");
    let (fallback, fallback_calls) = provider("fallback", "fallback-model", "from fallback");
    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .build();

    let stream = kernel.run_streaming(request("fallback-model"), None);
    tokio::pin!(stream);
    let mut final_text = None;
    while let Some(item) = stream.next().await {
        if let Event::Final(text) = item.expect("test result") {
            final_text = Some(text);
        }
    }

    assert_eq!(final_text.as_deref(), Some("from fallback"));
    assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn run_falls_back_through_provider_model_chain() {
    let (opus, opus_calls) = failing_provider("anthropic", "opus-4.6", server_error());
    let (gpt, gpt_calls) = failing_provider(
        "openai",
        "gpt-5.5",
        ProviderError::RateLimited {
            retry_after_secs: Some(5),
        },
    );
    let (haiku, haiku_calls) = provider("anthropic-lite", "haiku-4.5", "from haiku");
    let kernel = Kernel::builder()
        .provider(opus)
        .provider(gpt)
        .provider(haiku)
        .policy(AllowAllPolicy)
        .build();
    let mut req = request("opus-4.6");
    req.fallbacks = vec![
        ModelTarget::new("openai", "gpt-5.5"),
        ModelTarget::new("anthropic-lite", "haiku-4.5"),
    ];

    let text = kernel.run(req, None).await.expect("test result");

    assert_eq!(text, "from haiku");
    assert_eq!(opus_calls.load(Ordering::SeqCst), 1);
    assert_eq!(gpt_calls.load(Ordering::SeqCst), 1);
    assert_eq!(haiku_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fallback_can_try_another_model_on_same_provider() {
    let (anthropic, anthropic_calls) = catalog_provider(
        "anthropic",
        vec!["opus-4.6", "haiku-4.5"],
        vec![
            ProviderStep::Fail(server_error()),
            ProviderStep::Text("from haiku"),
        ],
    );
    let kernel = Kernel::builder()
        .provider(anthropic)
        .policy(AllowAllPolicy)
        .build();
    let mut req = request("opus-4.6");
    req.fallbacks = vec![ModelTarget::new("anthropic", "haiku-4.5")];

    let text = kernel.run(req, None).await.expect("test result");

    assert_eq!(text, "from haiku");
    assert_eq!(anthropic_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn thirty_second_rate_limit_does_not_fallback() {
    let (primary, primary_calls) = failing_provider(
        "primary",
        "primary-model",
        ProviderError::RateLimited {
            retry_after_secs: Some(30),
        },
    );
    let (fallback, fallback_calls) = provider("fallback", "fallback-model", "from fallback");
    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .build();
    let mut req = request("primary-model");
    req.fallbacks = vec![ModelTarget::new("fallback", "fallback-model")];

    let result = kernel.run(req, None).await;

    match result {
        Err(KernelError::Provider(ProviderError::RateLimited {
            retry_after_secs: Some(30),
        })) => {}
        other => panic!("expected short rate limit, got {other:?}"),
    }
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn fallback_target_routes_to_provider_model() {
    let (primary, primary_calls) = failing_provider("primary", "primary-model", server_error());
    let (openai, openai_calls) = provider("openai", "openai-model", "from openai");
    let (google, google_calls) = provider("google", "google-model", "wrong");
    let kernel = Kernel::builder()
        .provider(primary)
        .provider(openai)
        .provider(google)
        .policy(AllowAllPolicy)
        .build();
    let mut req = request("primary-model");
    req.fallbacks = vec![ModelTarget::new("openai", "openai-model")];

    let text = kernel.run(req, None).await.expect("test result");

    assert_eq!(text, "from openai");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(openai_calls.load(Ordering::SeqCst), 1);
    assert_eq!(google_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn streaming_falls_back_before_opening_stream() {
    let (primary, primary_calls) = failing_provider("primary", "primary-model", server_error());
    let (fallback, fallback_calls) = provider("fallback", "fallback-model", "from fallback");
    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .build();
    let mut req = request("primary-model");
    req.fallbacks = vec![ModelTarget::new("fallback", "fallback-model")];

    let stream = kernel.run_streaming(req, None);
    tokio::pin!(stream);
    let mut final_text = None;
    while let Some(item) = stream.next().await {
        if let Event::Final(text) = item.expect("test result") {
            final_text = Some(text);
        }
    }

    assert_eq!(final_text.as_deref(), Some("from fallback"));
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
}
