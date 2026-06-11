use std::sync::Arc;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    AllowAllPolicy, ContentBlock, Decision, Event, EventStream, Hook, Kernel, LlmRequest,
    LlmResponse, Message, ModelInfo, Provider, ProviderCapabilities, ProviderError, ProviderEvent,
    RunCtx, RunId, RunRequest, StopReason, Subject,
};

struct ReasoningProvider;

#[async_trait]
impl Provider for ReasoningProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Other("native stream expected".into()))
    }

    async fn stream(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<EventStream, ProviderError> {
        let events: Vec<Result<ProviderEvent, ProviderError>> = vec![
            Ok(ProviderEvent::ReasoningDelta("first".into())),
            Ok(ProviderEvent::ReasoningDelta("second".into())),
            Ok(ProviderEvent::Stop(StopReason::EndTurn)),
        ];
        Ok(Box::pin(stream::iter(events)))
    }

    fn name(&self) -> &'static str {
        "reasoning-stream"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("test", "reasoning-stream")]
    }
}

struct ReasoningRecorder {
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Hook for ReasoningRecorder {
    async fn on_event(&self, event: &Event, _ctx: &RunCtx) -> Decision<Event> {
        if let Event::Reasoning(detail) = event {
            self.events.lock().await.push(detail.clone());
        }
        Decision::Continue
    }
}

fn make_request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("reasoning-user"),
        model: "test".into(),
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

#[tokio::test]
async fn reasoning_delta_dispatches_to_event_and_hook() {
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(ReasoningProvider)
        .policy(AllowAllPolicy)
        .add_hook(ReasoningRecorder {
            events: Arc::clone(&recorded),
        })
        .build();

    let stream = kernel.run_streaming(make_request(), None);
    tokio::pin!(stream);
    let mut from_stream: Vec<String> = Vec::new();
    while let Some(item) = stream.next().await {
        match item.expect("stream item is Ok") {
            Event::Reasoning(detail) => from_stream.push(detail),
            Event::Final(_) => break,
            _ => {}
        }
    }

    assert_eq!(
        from_stream,
        vec!["first".to_string(), "second".to_string()],
        "kernel must re-emit ProviderEvent::ReasoningDelta as Event::Reasoning in order"
    );
    let captured = recorded.lock().await;
    assert_eq!(
        captured.as_slice(),
        &["first".to_string(), "second".to_string()],
        "Hook::on_event observes the same reasoning deltas in order"
    );
}
