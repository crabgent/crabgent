//! Regression: the non-streaming complete path must let the retry lifecycle
//! own the per-attempt timeout and must not cap the whole retry loop to a
//! single attempt's wall-clock with an outer `timeout(complete_timeout)`.

use std::time::Duration;

use crabgent_core::{LlmRequest, Provider};
use crabgent_provider_anthropic::{AnthropicConfig, AnthropicProvider};
use serde_json::json;

fn req(model: &str) -> LlmRequest {
    LlmRequest {
        model: model.into(),
        system_prompt: Some("be helpful".into()),
        messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})],
        tools: vec![],
        max_tokens: Some(64),
        temperature: None,
        stop_sequences: vec![],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    }
}

#[tokio::test]
async fn complete_retry_outlives_single_attempt_timeout_budget() {
    // Each HTTP attempt responds instantly, but the inter-attempt backoff
    // (200ms) alone exceeds `complete_timeout` (100ms). An outer guard around
    // the retry loop would fire a spurious `Timeout` during the backoff sleep;
    // without it, the second attempt succeeds.
    let mut server = mockito::Server::new_async().await;
    let _m500 = server
        .mock("POST", "/v1/messages")
        .with_status(500)
        .with_body("server error")
        .expect(1)
        .create_async()
        .await;
    let success = json!({
        "id": "msg_retry",
        "model": "claude",
        "content": [{"type": "text", "text": "recovered"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 5, "output_tokens": 1},
    });
    let _ok = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_body(success.to_string())
        .create_async()
        .await;

    let cfg = AnthropicConfig::new("k")
        .with_endpoint(server.url())
        .with_max_retries(1)
        .with_retry_base_delay(Duration::from_millis(200))
        .with_complete_timeout(Duration::from_millis(100));
    let provider = AnthropicProvider::try_new(reqwest::Client::new(), cfg).expect("valid config");

    let resp = provider
        .complete(
            &req("claude"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect("retry succeeds despite short per-attempt timeout");
    assert_eq!(resp.text, "recovered");
}
