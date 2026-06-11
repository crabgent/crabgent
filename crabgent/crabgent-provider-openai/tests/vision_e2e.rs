use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::{
    ContentBlock, ImagePayload, LlmRequest, LlmResponse, Message, ModelInfo, Provider,
    ProviderCapabilities, ProviderError, RawMessages, RunCtx, StopReason, Usage, WebSearchConfig,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,

        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Ok(LlmResponse {
            text: "ok".to_owned(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "stub"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            vision: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("gpt-5.5", "stub")]
    }
}

struct RecordingProvider<P> {
    inner: P,
    recorded: Arc<Mutex<Vec<LlmRequest>>>,
}

impl<P> RecordingProvider<P> {
    const fn new(inner: P, recorded: Arc<Mutex<Vec<LlmRequest>>>) -> Self {
        Self { inner, recorded }
    }
}

#[async_trait]
impl<P> Provider for RecordingProvider<P>
where
    P: Provider,
{
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,

        cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.recorded
            .lock()
            .expect("recording lock")
            .push(req.clone());
        self.inner
            .complete(
                req,
                &RunCtx::new(
                    crabgent_core::RunId::new(),
                    crabgent_core::Subject::new("test"),
                ),
                cancel,
            )
            .await
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.inner.models()
    }
}

#[tokio::test]
async fn vision_message_roundtrip() {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingProvider::new(StubProvider, Arc::clone(&recorded));
    let request = vision_request();

    let response = provider
        .complete(
            &request,
            &RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect("stub ok");

    assert_eq!(response.text, "ok");
    let captured = recorded.lock().expect("recording lock");
    assert_eq!(captured.len(), 1);
    let message: Message =
        serde_json::from_value(captured[0].messages[0].clone()).expect("typed message");
    let Message::User { content, .. } = message else {
        panic!("expected user message");
    };
    assert!(content.iter().any(|block| {
        matches!(
            block,
            ContentBlock::Image(payload)
                if payload.mime() == "image/png" && payload.bytes().as_ref() == b"png"
        )
    }));
}

fn vision_request() -> LlmRequest {
    let messages = RawMessages::from(vec![Message::User {
        content: vec![
            ContentBlock::Text {
                text: "describe".to_owned(),
            },
            ContentBlock::Image(
                ImagePayload::new(b"png".to_vec(), "image/png").expect("valid image payload"),
            ),
        ],
        timestamp: None,
    }])
    .into_inner();

    LlmRequest {
        model: "gpt-5.5".into(),
        system_prompt: None,
        messages,
        tools: Vec::new(),
        max_tokens: Some(64),
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    }
}
