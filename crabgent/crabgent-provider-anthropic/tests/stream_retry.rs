use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, LlmRequest, LlmResponse, Message, ModelInfo, ModelTarget,
    Provider, ProviderCapabilities, ProviderError, RunCtx, RunId, RunRequest, StopReason, Subject,
    Usage,
};
use crabgent_provider_anthropic::{AnthropicConfig, AnthropicProvider};
use tokio_util::sync::CancellationToken;

const RETRYABLE_SSE_ERROR: &str = "event: error\n\
data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"server overloaded\"}}\n\n";

const NON_RETRYABLE_SSE_ERROR: &str = "event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"echo\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{not-json\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n";

struct FallbackProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for FallbackProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,

        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            text: "fallback text".into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "fallback-test"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("fallback-model", "fallback-test")]
    }
}

fn anthropic_provider(endpoint: &str) -> AnthropicProvider {
    let cfg = AnthropicConfig::new("sk-ant-api03-test")
        .with_endpoint(endpoint)
        .with_max_retries(0)
        .with_retry_base_delay(Duration::from_millis(1));
    AnthropicProvider::try_new(reqwest::Client::new(), cfg).expect("valid config")
}

fn request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("user"),
        model: ModelTarget::new("anthropic", "claude-haiku-4-5"),
        explicit_model: None,
        session_model_override: None,
        fallbacks: vec![ModelTarget::new("fallback-test", "fallback-model")],
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
async fn retryable_stream_error_falls_back_to_next_provider() {
    let mut server = mockito::Server::new_async().await;
    let primary = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(RETRYABLE_SSE_ERROR)
        .create_async()
        .await;
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::builder()
        .provider(anthropic_provider(&server.url()))
        .provider(FallbackProvider {
            calls: Arc::clone(&fallback_calls),
        })
        .policy(AllowAllPolicy)
        .build();

    let text = kernel.run(request(), None).await.expect("run");

    assert_eq!(text, "fallback text");
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    primary.assert_async().await;
}

#[tokio::test]
async fn non_retryable_stream_error_does_not_fallback() {
    let mut server = mockito::Server::new_async().await;
    let primary = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(NON_RETRYABLE_SSE_ERROR)
        .create_async()
        .await;
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::builder()
        .provider(anthropic_provider(&server.url()))
        .provider(FallbackProvider {
            calls: Arc::clone(&fallback_calls),
        })
        .policy(AllowAllPolicy)
        .build();

    let err = kernel.run(request(), None).await.expect_err("run fails");

    match err {
        crabgent_core::KernelError::Provider(ProviderError::Other(message)) => {
            assert!(message.contains("malformed input JSON"));
        }
        other => panic!("expected ProviderError::Other, got {other:?}"),
    }
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
    primary.assert_async().await;
}
