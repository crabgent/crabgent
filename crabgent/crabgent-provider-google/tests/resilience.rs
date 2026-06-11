use std::time::Duration;

use crabgent_core::{LlmRequest, Provider, ProviderError, RunCtx, RunId, Subject};
use crabgent_provider_google::models::GEMINI_3_5_FLASH;
use crabgent_provider_google::{GoogleConfig, GoogleProvider};
use serde_json::{Value, json};

const API_KEY_SECRET: &str = "secret-test-google-key-99999";

fn config(base_url: &str) -> GoogleConfig {
    GoogleConfig::new(API_KEY_SECRET.to_owned())
        .with_base_url(base_url.to_owned())
        .with_max_retries(0)
        .with_retry_base_delay(Duration::from_millis(1))
        .with_request_timeout(Duration::from_secs(2))
}

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("test-subject"))
}

fn request_with_messages(messages: Vec<Value>) -> LlmRequest {
    LlmRequest {
        model: GEMINI_3_5_FLASH.into(),
        system_prompt: None,
        messages,
        tools: Vec::new(),
        max_tokens: None,
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    }
}

#[tokio::test]
async fn gemini_complete_drops_orphan_tool_pairs() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .match_header("x-goog-api-key", API_KEY_SECRET)
        .match_request(assert_orphan_cleanup_body)
        .with_status(200)
        .with_body(
            r#"{
                "candidates": [{
                    "finishReason": "STOP",
                    "content": {"parts": [{"text": "ok"}]}
                }]
            }"#,
        )
        .expect(1)
        .create_async()
        .await;
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");
    let request = request_with_messages(vec![
        json!({"role": "user", "content": "start"}),
        json!({
            "role": "assistant",
            "tool_calls": [
                {"id": "call_keep", "name": "lookup", "args": {"q": "kept"}},
                {"id": "call_drop", "name": "lookup", "args": {"q": "dropped"}}
            ],
        }),
        json!({"role": "tool_result", "call_id": "call_keep", "output": {"answer": 42}}),
        json!({
            "role": "assistant",
            "tool_calls": [
                {"id": "call_orphan", "name": "lookup", "args": {"q": "orphan"}}
            ],
        }),
        json!({"role": "tool_result", "call_id": "call_missing", "output": "orphan result"}),
        json!({"role": "user", "content": "done"}),
    ]);

    let response = provider
        .complete(&request, &ctx(), None)
        .await
        .expect("gemini response");

    mock.assert_async().await;
    assert_eq!(response.text, "ok");
}

#[tokio::test]
async fn gemini_rate_limit_preserves_retry_after() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .with_status(429)
        .with_header("retry-after", "45")
        .with_body("rate limited")
        .expect(1)
        .create_async()
        .await;
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");

    let err = provider
        .complete(
            &request_with_messages(vec![json!({"role": "user", "content": "hello"})]),
            &ctx(),
            None,
        )
        .await
        .expect_err("rate limit error");

    mock.assert_async().await;
    let ProviderError::RateLimited { retry_after_secs } = err else {
        panic!("unexpected error: {err:?}");
    };
    assert_eq!(retry_after_secs, Some(45));
}

fn assert_orphan_cleanup_body(request: &mockito::Request) -> bool {
    let Some(value) = body_json(request) else {
        return false;
    };
    let Some(contents) = value["contents"].as_array() else {
        return false;
    };
    let body = value.to_string();
    contents.len() == 4
        && !body.contains("call_drop")
        && !body.contains("call_orphan")
        && !body.contains("call_missing")
        && contents[0]["parts"][0]["text"] == "start"
        && contents[1]["role"] == "model"
        && contents[1]["parts"]
            .as_array()
            .is_some_and(|parts| parts.len() == 1)
        && contents[1]["parts"][0]["functionCall"]["id"] == "call_keep"
        && contents[1]["parts"][0]["functionCall"]["name"] == "lookup"
        && contents[2]["parts"][0]["functionResponse"]["id"] == "call_keep"
        && contents[2]["parts"][0]["functionResponse"]["name"] == "lookup"
        && contents[2]["parts"][0]["functionResponse"]["response"]["answer"] == 42
        && contents[3]["parts"][0]["text"] == "done"
}

fn body_json(request: &mockito::Request) -> Option<Value> {
    let body = request.utf8_lossy_body().ok()?;
    serde_json::from_str::<Value>(&body).ok()
}
