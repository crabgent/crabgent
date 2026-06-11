//! A broken or request-incompatible fallback must not abort a healthy primary.
//!
//! Regression: an operator configured a fallback to a deactivated model that
//! the registry no longer knew. The eager `resolve_attempts` pass turned the
//! unknown fallback into a `KernelError::UnknownModelTarget` before the primary
//! provider ever opened a stream, so a healthy primary turn died and the error
//! looked like the primary model was unavailable. The same asymmetry existed in
//! the eager capability pre-flight: a fallback that cannot serve the request
//! shape (e.g. no tool support while a tool is advertised) aborted the run too.
//!
//! Fallbacks are best-effort degradation: an unresolvable or incompatible
//! fallback is dropped from the chain, the healthy primary still runs, and a
//! primary that fails surfaces its own real error, never a fallback artifact.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, EventStream, Kernel, KernelError, LlmRequest, LlmResponse,
    Message, ModelInfo, ModelTarget, Provider, ProviderCapabilities, ProviderError, ProviderEvent,
    RunCtx, RunId, RunRequest, StopReason, Subject, Tool, ToolCtx, ToolError,
};
use futures::stream;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct HealthyPrimaryProvider {
    calls: Arc<AtomicUsize>,
    supports_tools: bool,
}

#[async_trait]
impl Provider for HealthyPrimaryProvider {
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
        let events: Vec<Result<ProviderEvent, ProviderError>> = vec![
            Ok(ProviderEvent::TextDelta("primary done".into())),
            Ok(ProviderEvent::Stop(StopReason::EndTurn)),
        ];
        Ok(Box::pin(stream::iter(events)))
    }

    fn name(&self) -> &'static str {
        "healthy-primary"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: self.supports_tools,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("primary-model", self.name())]
    }
}

/// Streams a fallback-eligible 503 on stream-open so the run exercises the
/// exhaustion contract when no fallback survives.
struct FailingPrimaryProvider {
    calls: Arc<AtomicUsize>,
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
        Err(ProviderError::Api {
            status: 503,
            message: "primary unavailable".into(),
            retry_after_secs: None,
        })
    }

    fn name(&self) -> &'static str {
        "failing-primary"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("primary-model", self.name())]
    }
}

/// A registered fallback whose provider does not advertise tool support. With a
/// tool advertised it fails the capability pre-flight and must be pruned, not
/// abort the primary.
struct NoToolsFallbackProvider;

#[async_trait]
impl Provider for NoToolsFallbackProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Other("fallback should never run".into()))
    }

    async fn stream(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<EventStream, ProviderError> {
        Err(ProviderError::Other("fallback should never run".into()))
    }

    fn name(&self) -> &'static str {
        "no-tools-fallback"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("fallback-model", self.name())]
    }
}

struct NamedTool(&'static str);

#[async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &'static str {
        self.0
    }

    fn description(&self) -> &'static str {
        "test tool"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(json!({"ok": true}))
    }
}

fn run_request_with_fallbacks(fallbacks: Vec<ModelTarget>) -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("fallback-fault-isolation-user"),
        model: ModelTarget::id("primary-model"),
        explicit_model: None,
        session_model_override: None,
        fallbacks,
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
        web_search: crabgent_core::types::WebSearchConfig::default(),
    }
}

#[tokio::test]
async fn unknown_fallback_target_does_not_abort_healthy_primary() {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::builder()
        .provider(HealthyPrimaryProvider {
            calls: Arc::clone(&primary_calls),
            supports_tools: false,
        })
        .policy(AllowAllPolicy)
        .build();

    // The fallback target points at a provider/model the registry never
    // learned about (a deactivated model). It must be dropped, not fatal.
    let req = run_request_with_fallbacks(vec![ModelTarget::new(
        "ghost-provider",
        "deactivated-model",
    )]);

    let text = kernel
        .run(req, None)
        .await
        .expect("healthy primary must run despite an unresolvable fallback");

    assert_eq!(text, "primary done");
    assert_eq!(
        primary_calls.load(Ordering::SeqCst),
        1,
        "primary provider must have been streamed exactly once",
    );
}

#[tokio::test]
async fn all_fallbacks_unresolvable_still_runs_primary() {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::builder()
        .provider(HealthyPrimaryProvider {
            calls: Arc::clone(&primary_calls),
            supports_tools: false,
        })
        .policy(AllowAllPolicy)
        .build();

    let req = run_request_with_fallbacks(vec![
        ModelTarget::new("ghost-one", "gone-a"),
        ModelTarget::new("ghost-two", "gone-b"),
    ]);

    let text = kernel
        .run(req, None)
        .await
        .expect("healthy primary must run despite all fallbacks being unresolvable");

    assert_eq!(text, "primary done");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn capability_incompatible_fallback_does_not_abort_healthy_primary() {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::builder()
        .provider(HealthyPrimaryProvider {
            calls: Arc::clone(&primary_calls),
            supports_tools: true,
        })
        .provider(NoToolsFallbackProvider)
        .policy(AllowAllPolicy)
        .add_tool(NamedTool("noop"))
        .build();

    // The fallback is registered and resolvable, but its provider cannot serve
    // a tool-advertising request. Eager capability pre-flight used to abort the
    // whole run here; the fallback must be pruned instead.
    let req = run_request_with_fallbacks(vec![ModelTarget::new(
        "no-tools-fallback",
        "fallback-model",
    )]);

    let text = kernel
        .run(req, None)
        .await
        .expect("healthy primary must run despite a tool-incompatible fallback");

    assert_eq!(text, "primary done");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn primary_failure_with_unknown_fallback_surfaces_primary_error() {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::builder()
        .provider(FailingPrimaryProvider {
            calls: Arc::clone(&primary_calls),
        })
        .policy(AllowAllPolicy)
        .build();

    // Primary fails with a fallback-eligible 503 and the only fallback is dead.
    // The surfaced error must be the primary's real error, never a synthesized
    // fallback artifact (UnknownModelTarget) or a generic exhaustion error.
    let req = run_request_with_fallbacks(vec![ModelTarget::new(
        "ghost-provider",
        "deactivated-model",
    )]);

    let err = kernel
        .run(req, None)
        .await
        .expect_err("primary 503 with a dead fallback must surface the primary error");

    assert!(
        matches!(
            err,
            KernelError::Provider(ProviderError::Api { status: 503, .. })
        ),
        "expected the primary's real 503, got {err:?}",
    );
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
}
