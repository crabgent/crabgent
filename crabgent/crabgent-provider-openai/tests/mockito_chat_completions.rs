use std::time::Duration;

use crabgent_core::{LlmRequest, Provider, ProviderEvent, StopReason, WebSearchConfig};
use crabgent_provider_openai::{ApiKeyAuth, OpenAiConfig, OpenAiProvider};
use futures::StreamExt;
use mockito::Matcher;
use secrecy::SecretString;
use serde_json::json;

const API_KEY_SECRET: &str = "secret-test-key-99999";

fn api_key_auth(base_url: &str) -> ApiKeyAuth {
    ApiKeyAuth::new(SecretString::from(API_KEY_SECRET.to_owned()))
        .with_base_url(base_url.to_owned())
}

fn config() -> OpenAiConfig {
    OpenAiConfig::new("secret-test-key-99999")
        .with_max_retries(0)
        .with_retry_base_delay(Duration::from_millis(1))
        .with_request_timeout(Duration::from_secs(2))
}

fn req() -> LlmRequest {
    LlmRequest {
        model: "gpt-5.5".into(),
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
async fn complete_chat_completions_parses_response() {
    let mut server = mockito::Server::new_async().await;
    let auth = api_key_auth(&server.url());
    let body = json!({
        "model": "gpt-5.5",
        "choices": [{
            "message": {"content": "hello from chat"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2}
    });
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_header(
            "authorization",
            Matcher::Exact(format!("Bearer {API_KEY_SECRET}")),
        )
        .match_header("content-type", "application/json")
        .match_body(Matcher::PartialJson(json!({
            "model": "gpt-5.5",
            "stream": false
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body.to_string())
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

    assert_eq!(response.text, "hello from chat");
    assert!(matches!(response.stop_reason, StopReason::EndTurn));
    assert_eq!(response.usage.input_tokens, 3);
    assert_eq!(response.usage.output_tokens, 2);
    mock.assert_async().await;
}

#[tokio::test]
async fn stream_chat_completions_emits_events() {
    let mut server = mockito::Server::new_async().await;
    let auth = api_key_auth(&server.url());
    let stream_body = "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n";
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_header(
            "authorization",
            Matcher::Exact(format!("Bearer {API_KEY_SECRET}")),
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
    let mut saw_stop = false;
    while let Some(event) = events.next().await {
        match event.expect("event ok") {
            ProviderEvent::TextDelta(delta) => text.push_str(&delta),
            ProviderEvent::Stop(StopReason::EndTurn) => {
                saw_stop = true;
                break;
            }
            _ => {}
        }
    }

    assert_eq!(text, "hello");
    assert!(saw_stop);
    mock.assert_async().await;
}

#[tokio::test]
async fn complete_with_audio_input_sends_input_audio_and_parses_answer() {
    let mut server = mockito::Server::new_async().await;
    let auth = api_key_auth(&server.url());
    let body = json!({
        "model": "gpt-4o-audio-preview",
        "choices": [{
            "message": {"content": "they said hello"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 7, "completion_tokens": 3}
    });
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_body(Matcher::AllOf(vec![
            Matcher::PartialJson(json!({"model": "gpt-4o-audio-preview"})),
            Matcher::Regex("input_audio".to_owned()),
        ]))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body.to_string())
        .create_async()
        .await;

    let mut request = req();
    request.model = "gpt-4o-audio-preview".into();
    request.messages = vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "what did they say?"},
            {"type": "audio", "mime": "audio/wav", "data": "UklGRiQ="}
        ],
    })];

    let provider = OpenAiProvider::try_new(reqwest::Client::new(), config(), Box::new(auth))
        .expect("valid provider");
    let response = provider
        .complete(
            &request,
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect("complete ok");

    assert_eq!(response.text, "they said hello");
    mock.assert_async().await;
}
