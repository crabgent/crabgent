use std::time::Duration;

use crabgent_core::{LlmRequest, Provider, ProviderEvent, StopReason, WebSearchConfig};
use crabgent_provider_openai::{CodexOAuthAuth, OpenAiConfig, OpenAiProvider};
use futures::StreamExt;
use mockito::Matcher;
use secrecy::SecretString;
use serde_json::json;

const CODEX_TOKEN_SECRET: &str = "secret-test-token-99999";
const CODEX_ACCOUNT_ID: &str = "account-test-id";

fn codex_auth(base_url: &str) -> CodexOAuthAuth {
    CodexOAuthAuth::new(
        SecretString::from(CODEX_TOKEN_SECRET.to_owned()),
        Some(CODEX_ACCOUNT_ID.to_owned()),
    )
    .with_base_url(base_url.to_owned())
}

fn config() -> OpenAiConfig {
    OpenAiConfig::new("secret-config-key")
        .with_max_retries(0)
        .with_retry_base_delay(Duration::from_millis(1))
        .with_request_timeout(Duration::from_secs(2))
}

fn req() -> LlmRequest {
    LlmRequest {
        model: "gpt-5.3-codex".into(),
        system_prompt: Some("be precise".to_owned()),
        messages: vec![json!({
            "role": "user",
            "content": [{"type": "text", "text": "hi"}],
        })],
        tools: Vec::new(),
        max_tokens: Some(64),
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    }
}

#[tokio::test]
async fn complete_responses_uses_streaming_under_codex_auth() {
    // Codex backend rejects `stream=false` with HTTP 400
    // "Stream must be set to true". `Provider::complete` must therefore
    // open the streaming path internally and accumulate the SSE events
    // back into an `LlmResponse`. Regression for that wire constraint.
    let mut server = mockito::Server::new_async().await;
    let auth = codex_auth(&server.url());
    let stream_body = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi from \"}\n\n\
data: {\"type\":\"response.output_text.delta\",\"delta\":\"codex\"}\n\n\
data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":4,\"output_tokens\":2}}}\n\n";
    let mock = server
        .mock("POST", "/backend-api/codex/responses")
        .match_header(
            "authorization",
            Matcher::Exact(format!("Bearer {CODEX_TOKEN_SECRET}")),
        )
        .match_header("openai-beta", "responses=experimental")
        .match_header("originator", "codex_cli_rs")
        .match_header("user-agent", "codex_cli_rs/0.59.0")
        .match_header("chatgpt-account-id", CODEX_ACCOUNT_ID)
        .match_body(Matcher::PartialJson(json!({
            "instructions": "be precise",
            "stream": true,
            "store": false
        })))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(stream_body)
        .create_async()
        .await;

    let provider = OpenAiProvider::try_new(reqwest::Client::new(), config(), Box::new(auth))
        .expect("valid provider");
    let response = provider
        .complete(
            &req(),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect("complete ok");

    assert_eq!(response.text, "hi from codex");
    assert!(matches!(response.stop_reason, StopReason::EndTurn));
    assert_eq!(response.usage.input_tokens, 4);
    assert_eq!(response.usage.output_tokens, 2);
    mock.assert_async().await;
}

#[tokio::test]
async fn stream_responses_emits_events() {
    let mut server = mockito::Server::new_async().await;
    let auth = codex_auth(&server.url());
    let stream_body = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n\
data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n";
    let mock = server
        .mock("POST", "/backend-api/codex/responses")
        .match_header(
            "authorization",
            Matcher::Exact(format!("Bearer {CODEX_TOKEN_SECRET}")),
        )
        .match_body(Matcher::PartialJson(json!({"stream": true})))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(stream_body)
        .create_async()
        .await;

    let provider = OpenAiProvider::try_new(reqwest::Client::new(), config(), Box::new(auth))
        .expect("valid provider");
    let mut events = provider
        .stream(
            &req(),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect("stream ok");
    let mut text = String::new();
    let mut saw_usage = false;
    let mut saw_stop = false;
    while let Some(event) = events.next().await {
        match event.expect("event ok") {
            ProviderEvent::TextDelta(delta) => text.push_str(&delta),
            ProviderEvent::Usage(usage) => saw_usage = usage.input_tokens == 2,
            ProviderEvent::Stop(StopReason::EndTurn) => {
                saw_stop = true;
                break;
            }
            _ => {}
        }
    }

    assert_eq!(text, "hi");
    assert!(saw_usage);
    assert!(saw_stop);
    mock.assert_async().await;
}
