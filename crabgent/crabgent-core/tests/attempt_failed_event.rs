//! Observable fallback transitions via `Event::AttemptFailed`.
//!
//! The kernel must push an `Event::AttemptFailed` through the hook chain on
//! every failed provider attempt so operators (via `crabgent-hook-log`) can
//! tell why a chain fell back, retried, or terminated.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, AttemptErrorClass, ContentBlock, Decision, Event, EventStream, Hook, Kernel,
    KernelError, LlmRequest, LlmResponse, Message, ModelInfo, ModelTarget, Provider,
    ProviderCapabilities, ProviderError, ProviderEvent, RunCtx, RunId, RunRequest, StopReason,
    Subject,
};
use futures::stream;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
enum PrimaryFailure {
    /// Stream-open returns a 503 (fallback-eligible API server error).
    OpenServerError,
    /// Stream-open returns Auth (terminal, kills the chain).
    OpenAuth,
}

struct FailingPrimaryProvider {
    failure: PrimaryFailure,
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
        match self.failure {
            PrimaryFailure::OpenServerError => Err(ProviderError::Api {
                status: 503,
                message: "primary unavailable".into(),
                retry_after_secs: None,
            }),
            PrimaryFailure::OpenAuth => Err(ProviderError::Auth("bad key".into())),
        }
    }

    fn name(&self) -> &'static str {
        "primary-attempt"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("primary-model", self.name())]
    }
}

struct StaticFallbackProvider;

#[async_trait]
impl Provider for StaticFallbackProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Other("stream-only fallback".into()))
    }

    async fn stream(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<EventStream, ProviderError> {
        let events: Vec<Result<ProviderEvent, ProviderError>> = vec![
            Ok(ProviderEvent::TextDelta("fallback done".into())),
            Ok(ProviderEvent::Stop(StopReason::EndTurn)),
        ];
        let _ = req;
        Ok(Box::pin(stream::iter(events)))
    }

    fn name(&self) -> &'static str {
        "fallback-attempt"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("fallback-model", self.name())]
    }
}

type RecordedEvents = Arc<std::sync::Mutex<Vec<Event>>>;

struct RecordingEventHook {
    events: RecordedEvents,
}

#[async_trait]
impl Hook for RecordingEventHook {
    async fn on_event(&self, ev: &Event, _ctx: &RunCtx) -> Decision<Event> {
        self.events
            .lock()
            .expect("recording event hook lock poisoned")
            .push(ev.clone());
        Decision::Continue
    }
}

fn snapshot(events: &RecordedEvents) -> Vec<Event> {
    events
        .lock()
        .expect("recording event hook lock poisoned")
        .clone()
}

fn run_request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("attempt-failed-user"),
        model: ModelTarget::id("primary-model"),
        explicit_model: None,
        session_model_override: None,
        fallbacks: vec![ModelTarget::new("fallback-attempt", "fallback-model")],
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
async fn attempt_failed_event_emitted_when_attempt_errors_with_fallback_eligible() {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let events: RecordedEvents = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(FailingPrimaryProvider {
            failure: PrimaryFailure::OpenServerError,
            calls: Arc::clone(&primary_calls),
        })
        .provider(StaticFallbackProvider)
        .add_hook(RecordingEventHook {
            events: Arc::clone(&events),
        })
        .policy(AllowAllPolicy)
        .build();

    let text = kernel
        .run(run_request(), None)
        .await
        .expect("fallback should succeed");
    assert_eq!(text, "fallback done");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);

    let events = snapshot(&events);
    let attempt_failed = events
        .iter()
        .find(|ev| matches!(ev, Event::AttemptFailed { .. }))
        .expect("AttemptFailed event must be recorded");

    match attempt_failed {
        Event::AttemptFailed {
            attempt_idx,
            total_attempts,
            provider,
            model,
            error_class,
            message,
            will_fallback,
        } => {
            assert_eq!(*attempt_idx, 0);
            assert_eq!(*total_attempts, 2);
            assert_eq!(provider, "primary-attempt");
            assert_eq!(model, "primary-model");
            assert_eq!(error_class, &AttemptErrorClass::ApiServer { status: 503 });
            assert!(*will_fallback, "503 must be marked fallback-eligible");
            assert!(
                !message.is_empty(),
                "message must carry the provider-error detail",
            );
            assert!(
                message.contains("503"),
                "message should mention the status code: {message:?}",
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }

    assert!(
        events.iter().any(|ev| matches!(ev, Event::Final(_))),
        "successful run must emit Final after fallback",
    );
}

#[tokio::test]
async fn attempt_failed_event_marked_terminal_when_error_kills_chain() {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let events: RecordedEvents = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(FailingPrimaryProvider {
            failure: PrimaryFailure::OpenAuth,
            calls: Arc::clone(&primary_calls),
        })
        .provider(StaticFallbackProvider)
        .add_hook(RecordingEventHook {
            events: Arc::clone(&events),
        })
        .policy(AllowAllPolicy)
        .build();

    let err = kernel
        .run(run_request(), None)
        .await
        .expect_err("Auth must terminate the chain");
    assert!(
        matches!(err, KernelError::Provider(ProviderError::Auth(_))),
        "expected Provider(Auth), got {err:?}",
    );
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);

    let events = snapshot(&events);
    let attempt_failed = events
        .iter()
        .find(|ev| matches!(ev, Event::AttemptFailed { .. }))
        .expect("AttemptFailed event must be recorded before terminal error");

    match attempt_failed {
        Event::AttemptFailed {
            attempt_idx,
            total_attempts,
            provider,
            model,
            error_class,
            message,
            will_fallback,
        } => {
            assert_eq!(*attempt_idx, 0);
            assert_eq!(*total_attempts, 2);
            assert_eq!(provider, "primary-attempt");
            assert_eq!(model, "primary-model");
            assert_eq!(error_class, &AttemptErrorClass::Auth);
            assert!(!*will_fallback, "Auth must terminate the chain");
            assert!(
                !message.is_empty(),
                "message must carry the provider-error detail",
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }

    assert!(
        !events.iter().any(|ev| matches!(ev, Event::Final(_))),
        "terminal run must not emit Final",
    );
}
