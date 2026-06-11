//! Integration tests for `AnthropicProvider` using a mockito HTTP server.

use std::time::Duration;
use std::time::Instant;

use crabgent_core::{LlmRequest, Provider, ProviderError, ProviderEvent, StopReason};
use crabgent_provider_anthropic::{AnthropicConfig, AnthropicProvider};
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

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

fn provider(endpoint: &str) -> AnthropicProvider {
    let cfg = AnthropicConfig::new("sk-ant-api03-test")
        .with_endpoint(endpoint)
        .with_max_retries(0)
        .with_retry_base_delay(Duration::from_millis(1));
    AnthropicProvider::try_new(reqwest::Client::new(), cfg).expect("valid config")
}

#[tokio::test]
async fn complete_parses_text_response() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "id": "msg_1",
        "model": "claude-haiku-4-5",
        "content": [{"type": "text", "text": "hello world"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 5, "output_tokens": 2},
    });
    let _m = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body.to_string())
        .create_async()
        .await;

    let provider = provider(&server.url());
    let resp = provider
        .complete(
            &req("claude-haiku-4-5"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect("complete ok");
    assert_eq!(resp.text, "hello world");
    assert!(matches!(resp.stop_reason, StopReason::EndTurn));
    assert_eq!(resp.usage.input_tokens, 5);
    assert_eq!(resp.usage.output_tokens, 2);
}

#[tokio::test]
async fn complete_returns_auth_error_on_401() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/v1/messages")
        .with_status(401)
        .with_body(r#"{"error":{"type":"invalid_api_key","message":"bad key"}}"#)
        .create_async()
        .await;

    let provider = provider(&server.url());
    let r = provider
        .complete(
            &req("claude"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await;
    match r {
        Err(ProviderError::Auth(msg)) => assert_eq!(msg, "anthropic authentication failed"),
        other => panic!("expected Auth, got {other:?}"),
    }
}

#[tokio::test]
async fn auth_failure_no_key_leak() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/v1/messages")
        .with_status(401)
        .with_body("key was sk-ant-secret-test-99999")
        .create_async()
        .await;

    let provider = provider(&server.url());
    let err = provider
        .complete(
            &req("claude"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect_err("auth failure");
    let display = err.to_string();
    assert!(matches!(err, ProviderError::Auth(_)));
    assert!(!display.contains("secret-test"));
}

#[tokio::test]
async fn complete_returns_rate_limited_on_429() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/v1/messages")
        .with_status(429)
        .with_header("retry-after", "12")
        .with_body("rate limit hit")
        .create_async()
        .await;

    let provider = provider(&server.url());
    let r = provider
        .complete(
            &req("claude"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await;
    match r {
        Err(ProviderError::RateLimited { retry_after_secs }) => {
            assert_eq!(retry_after_secs, Some(12));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn complete_returns_api_error_on_400() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/v1/messages")
        .with_status(400)
        .with_body("max_tokens: 32768 > 32000 with secret-test-ant-key-99999")
        .create_async()
        .await;

    let provider = provider(&server.url());
    let err = provider
        .complete(
            &req("claude"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect_err("api failure");
    let display = err.to_string();
    match err {
        ProviderError::Api {
            status: 400,
            message,
            ..
        } => {
            assert!(message.contains("max_tokens: 32768 > 32000"));
            assert!(!message.contains("secret-test-ant-key-99999"));
        }
        other => panic!("expected Api 400, got {other:?}"),
    }
    assert!(!display.contains("secret-test"));
}

#[tokio::test]
async fn complete_retries_on_500_then_succeeds() {
    let mut server = mockito::Server::new_async().await;
    let _m500 = server
        .mock("POST", "/v1/messages")
        .with_status(500)
        .with_body("server error")
        .expect(1)
        .create_async()
        .await;
    let success = json!({
        "id": "msg_2",
        "model": "claude",
        "content": [{"type": "text", "text": "ok"}],
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
        .with_max_retries(2)
        .with_retry_base_delay(Duration::from_millis(1));
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
        .expect("ok");
    assert_eq!(resp.text, "ok");
}

#[tokio::test]
async fn complete_timeout_fires_on_slow_request_body() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "id": "msg_timeout",
        "model": "claude-haiku-4-5",
        "content": [{"type": "text", "text": "late"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 5, "output_tokens": 2},
    })
    .to_string();
    let _m = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_chunked_body(move |writer| {
            std::thread::sleep(Duration::from_millis(200));
            writer.write_all(body.as_bytes())
        })
        .create_async()
        .await;
    let cfg = AnthropicConfig::new("k")
        .with_endpoint(server.url())
        .with_max_retries(0)
        .with_complete_timeout(Duration::from_millis(50));
    let provider = AnthropicProvider::try_new(reqwest::Client::new(), cfg).expect("valid config");

    let started = Instant::now();
    let result = provider
        .complete(
            &req("claude"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await;

    assert!(matches!(result, Err(ProviderError::Timeout)));
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[tokio::test]
async fn complete_cancels_when_token_already_cancelled() {
    let server = mockito::Server::new_async().await;
    let provider = provider(&server.url());
    let token = CancellationToken::new();
    token.cancel();
    let r = provider
        .complete(
            &req("claude"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            Some(&token),
        )
        .await;
    assert!(matches!(r, Err(ProviderError::Cancelled)));
}

const SSE_TEXT_STREAM: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":1}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";

#[tokio::test]
async fn stream_emits_text_deltas_and_stop() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(SSE_TEXT_STREAM)
        .create_async()
        .await;

    let provider = provider(&server.url());
    let mut events = provider
        .stream(
            &req("claude"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect("stream ok");
    let mut text = String::new();
    let mut saw_stop = false;
    while let Some(ev) = events.next().await {
        match ev.expect("event ok") {
            ProviderEvent::TextDelta(s) => text.push_str(&s),
            ProviderEvent::Stop(StopReason::EndTurn) => {
                saw_stop = true;
                break;
            }
            _ => {}
        }
    }
    assert_eq!(text, "Hello world");
    assert!(saw_stop);
}

#[tokio::test]
async fn stream_body_is_not_capped_by_complete_timeout() {
    let mut server = mockito::Server::new_async().await;
    let first = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
    );
    let second = concat!(
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"late\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let _m = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_chunked_body(move |writer| {
            writer.write_all(first.as_bytes())?;
            std::thread::sleep(Duration::from_millis(120));
            writer.write_all(second.as_bytes())
        })
        .create_async()
        .await;
    let cfg = AnthropicConfig::new("k")
        .with_endpoint(server.url())
        .with_max_retries(0)
        .with_complete_timeout(Duration::from_millis(50));
    let provider = AnthropicProvider::try_new(reqwest::Client::new(), cfg).expect("valid config");

    let mut events = provider
        .stream(
            &req("claude"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect("stream should open before complete timeout");
    let mut text = String::new();
    while let Some(ev) = events.next().await {
        match ev.expect("event ok") {
            ProviderEvent::TextDelta(delta) => text.push_str(&delta),
            ProviderEvent::Stop(StopReason::EndTurn) => break,
            _ => {}
        }
    }

    assert_eq!(text, "late");
}

const SSE_TOOL_STREAM: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\"}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"echo\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"x\\\":1}\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":3}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";

#[tokio::test]
async fn stream_emits_tool_use_with_full_input() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(SSE_TOOL_STREAM)
        .create_async()
        .await;

    let provider = provider(&server.url());
    let mut events = provider
        .stream(
            &req("claude"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect("stream ok");
    let mut tool_id = None;
    let mut tool_args = None;
    let mut stop = None;
    while let Some(ev) = events.next().await {
        match ev.expect("event ok") {
            ProviderEvent::ToolUse(call) => {
                tool_id = Some(call.id);
                tool_args = Some(call.args);
            }
            ProviderEvent::Stop(s) => {
                stop = Some(s);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(tool_id.as_deref(), Some("toolu_1"));
    assert_eq!(tool_args, Some(json!({"x": 1})));
    assert!(matches!(stop, Some(StopReason::ToolUse)));
}

#[tokio::test]
async fn stream_propagates_auth_error_at_open() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/v1/messages")
        .with_status(401)
        .with_body("bad key")
        .create_async()
        .await;

    let provider = provider(&server.url());
    let r = provider
        .stream(
            &req("claude"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await;
    assert!(matches!(r, Err(ProviderError::Auth(_))));
}
