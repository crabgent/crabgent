use std::time::Duration;

mod support;

use crabgent_core::{ImageGenerationProvider, LlmRequest, Provider, ProviderError, StopReason};
use crabgent_provider_google::GoogleError;
use crabgent_provider_google::models::{GEMINI_3_1_FLASH_IMAGE_PREVIEW, GEMINI_3_5_FLASH};
use crabgent_provider_google::{GoogleConfig, GoogleImageGenerationProvider, GoogleProvider};
use serde_json::json;
use support::{
    API_KEY_SECRET, assert_cache_create_body, assert_cached_generate_body, assert_llm_body,
    assert_multimodal_body, config, ctx, llm_req,
};
use tokio_util::sync::CancellationToken;

#[test]
fn google_config_debug_masks_key_and_builders() {
    let cfg = GoogleConfig::new(API_KEY_SECRET.to_owned())
        .with_base_url("http://localhost:4444".to_owned())
        .with_api_version("/v1/".to_owned())
        .with_max_retries(2)
        .with_retry_base_delay(Duration::from_millis(7))
        .with_request_timeout(Duration::from_secs(3));

    let rendered = format!("{cfg:?}");

    assert!(rendered.contains("****<masked>"));
    assert!(!rendered.contains(API_KEY_SECRET));
    assert_eq!(cfg.base_url, "http://localhost:4444");
    assert_eq!(cfg.api_version, "/v1/");
    assert_eq!(cfg.max_retries, 2);
    assert_eq!(cfg.retry_base_delay, Duration::from_millis(7));
    assert_eq!(cfg.request_timeout, Duration::from_secs(3));
}

#[test]
fn google_provider_exposes_metadata() {
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config("http://localhost"))
        .expect("valid provider");

    assert_eq!(provider.name(), "google");
    assert_eq!(provider.config().base_url, "http://localhost");
    let _client = provider.http_client();

    let capabilities = provider.capabilities();
    assert!(capabilities.streaming);
    assert!(capabilities.tools);
    assert!(capabilities.vision);
    assert!(capabilities.audio);
    assert!(capabilities.thinking);
    assert!(capabilities.prompt_cache);
    assert!(capabilities.web_search);

    let models = provider.models();
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert!(models.len() >= 8);
    assert!(ids.contains(&GEMINI_3_5_FLASH));
    assert!(ids.contains(&"gemini-2.5-pro"));
}

#[test]
fn google_image_provider_exposes_metadata() {
    let image_provider =
        GoogleImageGenerationProvider::try_new(reqwest::Client::new(), config("http://localhost"))
            .expect("valid image provider");
    assert_eq!(image_provider.config().base_url, "http://localhost");

    let image_capabilities = image_provider.image_generation_capabilities();
    assert!(image_capabilities.generation);
    assert!(!image_capabilities.editing);

    let image_models = image_provider.image_generation_models();
    let ids: Vec<&str> = image_models.iter().map(|model| model.id.as_str()).collect();
    assert!(ids.contains(&GEMINI_3_1_FLASH_IMAGE_PREVIEW));
    assert!(ids.contains(&crabgent_provider_google::models::GEMINI_3_PRO_IMAGE_PREVIEW));
}

#[test]
fn google_provider_validates_config() {
    let Err(empty_key) = GoogleProvider::try_new(
        reqwest::Client::new(),
        config("http://localhost").with_api_version(String::new()),
    ) else {
        panic!("api version must be validated");
    };
    assert!(matches!(empty_key, GoogleError::ConfigError(_)));
}

#[tokio::test]
async fn google_provider_new_uses_static_models_when_discovery_fails() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/v1beta/models")
        .match_header("x-goog-api-key", API_KEY_SECRET)
        .with_status(500)
        .with_body(format!("backend body {API_KEY_SECRET}"))
        .expect(1)
        .create_async()
        .await;

    let provider = GoogleProvider::new(config(&server.url()))
        .await
        .expect("fallback provider");

    mock.assert_async().await;
    assert!(
        provider
            .models()
            .iter()
            .map(|model| model.id.as_str())
            .any(|id| id == GEMINI_3_5_FLASH)
    );
}

#[tokio::test]
async fn gemini_complete_posts_generate_content_and_parses_response() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .match_header("x-goog-api-key", API_KEY_SECRET)
        .match_request(assert_llm_body)
        .with_status(200)
        .with_body(
            r#"{
                "modelVersion": "gemini-3.5-flash",
                "candidates": [{
                    "finishReason": "MALFORMED_FUNCTION_CALL",
                    "content": {"parts": [
                        {"text": "hello"},
                        {
                            "functionCall": {"id": "call_lookup_1", "name": "lookup", "args": {"q": "x"}},
                            "thoughtSignature": "opaque-signature"
                        }
                    ]}
                }],
                "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 9}
            }"#,
        )
        .expect(1)
        .create_async()
        .await;
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");

    let response = provider
        .complete(&llm_req(), &ctx(), None)
        .await
        .expect("gemini response");

    mock.assert_async().await;
    assert_eq!(response.text, "hello");
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].id, "call_lookup_1");
    assert_eq!(response.tool_calls[0].name, "lookup");
    assert_eq!(
        response.tool_calls[0].thought_signature.as_deref(),
        Some("opaque-signature")
    );
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert_eq!(response.usage.input_tokens, 7);
    assert_eq!(response.usage.output_tokens, 9);
}

#[tokio::test]
async fn gemini_complete_reuses_created_cached_content() {
    let mut server = mockito::Server::new_async().await;
    let cache_mock = server
        .mock("POST", "/v1beta/cachedContents")
        .match_header("x-goog-api-key", API_KEY_SECRET)
        .match_request(assert_cache_create_body)
        .with_status(200)
        .with_body(
            r#"{
                "name": "cachedContents/test-cache",
                "expireTime": "2099-01-01T00:00:00Z",
                "usageMetadata": {"totalTokenCount": 17}
            }"#,
        )
        .expect(1)
        .create_async()
        .await;
    let generate_mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .match_header("x-goog-api-key", API_KEY_SECRET)
        .match_request(assert_cached_generate_body)
        .with_status(200)
        .with_body(
            r#"{
                "modelVersion": "gemini-3.5-flash",
                "candidates": [{
                    "finishReason": "STOP",
                    "content": {"parts": [{"text": "cached"}]}
                }],
                "usageMetadata": {
                    "promptTokenCount": 21,
                    "candidatesTokenCount": 3,
                    "cachedContentTokenCount": 13
                }
            }"#,
        )
        .expect(2)
        .create_async()
        .await;
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");

    let first = provider
        .complete(&llm_req(), &ctx(), None)
        .await
        .expect("first response");
    let second = provider
        .complete(&llm_req(), &ctx(), None)
        .await
        .expect("second response");

    cache_mock.assert_async().await;
    generate_mock.assert_async().await;
    assert_eq!(first.text, "cached");
    assert_eq!(first.usage.cache_creation_tokens, 17);
    assert_eq!(first.usage.cache_read_tokens, 13);
    assert_eq!(second.usage.cache_creation_tokens, 0);
    assert_eq!(second.usage.cache_read_tokens, 13);
}

#[tokio::test]
async fn gemini_complete_transforms_multimodal_history_and_defaults() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .match_header("x-goog-api-key", API_KEY_SECRET)
        .match_request(assert_multimodal_body)
        .with_status(200)
        .with_body(
            r#"{
                "candidates": [{
                    "finishReason": "MAX_TOKENS",
                    "content": {"parts": [{"text": "limited"}]}
                }]
            }"#,
        )
        .expect(1)
        .create_async()
        .await;
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");
    let request = LlmRequest {
        model: GEMINI_3_5_FLASH.into(),
        system_prompt: None,
        messages: vec![
            json!({"role": "system", "content": "ignored"}),
            json!({
                "role": "user",
                "content": [
                    {"type": "image", "mime": "image/png", "data": "encoded-image"},
                    {"type": "unknown"}
                ],
            }),
            json!({
                "role": "assistant",
                "text": "previous",
                "tool_calls": [{
                    "id": "call_lookup_old",
                    "name": "lookup",
                    "args": {"q": "old"},
                    "thought_signature": "signature-old"
                }],
            }),
            json!({"role": "tool_result", "call_id": "call_lookup_old", "output": {"answer": 42}}),
            json!({"role": "user", "content": "plain text"}),
            json!({"role": "channel_outbound", "content": "ignored"}),
        ],
        tools: Vec::new(),
        max_tokens: None,
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    };

    let response = provider
        .complete(&request, &ctx(), None)
        .await
        .expect("gemini response");

    mock.assert_async().await;
    assert_eq!(response.text, "limited");
    assert_eq!(response.stop_reason, StopReason::MaxTokens);
    assert_eq!(response.model.as_str(), GEMINI_3_5_FLASH);
    assert_eq!(response.usage.input_tokens, 0);
}

#[tokio::test]
async fn fetch_models_keeps_gemini_chat_variants() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/v1beta/models")
        .match_header("x-goog-api-key", API_KEY_SECRET)
        .with_status(200)
        .with_body(
            r#"{"models": [
                {"name": "models/gemini-3.5-flash"},
                {"name": "models/gemini-3.5-pro-preview"},
                {"name": "models/gemini-3.1-flash-lite"},
                {"name": "models/gemini-2.5-pro"},
                {"name": "models/gemini-3-pro-image-preview"},
                {"name": "models/gemini-embedding-001"},
                {"name": "models/gemini-2.5-pro-preview-tts"}
            ]}"#,
        )
        .expect(1)
        .create_async()
        .await;
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");

    let models = provider.fetch_models().await.expect("models");

    mock.assert_async().await;
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert!(ids.contains(&"gemini-3.5-flash"));
    assert!(ids.contains(&"gemini-3.5-pro-preview"));
    assert!(ids.contains(&"gemini-3.1-flash-lite"));
    assert!(ids.contains(&"gemini-2.5-pro"));
    assert!(!ids.contains(&"gemini-3-pro-image-preview"));
    assert!(!ids.contains(&"gemini-embedding-001"));
    assert!(!ids.contains(&"gemini-2.5-pro-preview-tts"));
}

#[tokio::test]
async fn fetch_models_rejects_malformed_json() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/v1beta/models")
        .match_header("x-goog-api-key", API_KEY_SECRET)
        .with_status(200)
        .with_body("not-json")
        .expect(1)
        .create_async()
        .await;
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");

    let err = provider
        .fetch_models()
        .await
        .expect_err("malformed discovery response");

    mock.assert_async().await;
    assert!(matches!(err, ProviderError::ModelDiscovery { .. }));
}

#[tokio::test]
async fn gemini_auth_failure_no_key_leak() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .with_status(401)
        .with_body(format!("bad key {API_KEY_SECRET}"))
        .expect(1)
        .create_async()
        .await;
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");

    let err = provider
        .complete(&llm_req(), &ctx(), None)
        .await
        .expect_err("auth error");

    mock.assert_async().await;
    assert!(matches!(err, ProviderError::Auth(_)));
    let rendered = format!("{err:?}\n{err}");
    assert!(
        !rendered.contains(API_KEY_SECRET),
        "secret leaked: {rendered}"
    );
    assert!(!rendered.contains("401"), "status leaked: {rendered}");
}

#[tokio::test]
async fn gemini_auth_with_large_body_still_maps_to_auth() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .with_status(401)
        .with_body("x".repeat(70_000))
        .expect(1)
        .create_async()
        .await;
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");

    let err = provider
        .complete(&llm_req(), &ctx(), None)
        .await
        .expect_err("auth error");

    mock.assert_async().await;
    assert!(matches!(err, ProviderError::Auth(_)));
}

#[tokio::test]
async fn gemini_complete_backend_error_is_opaque() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .with_status(500)
        .with_body(format!("backend body {API_KEY_SECRET}"))
        .expect(1)
        .create_async()
        .await;
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");

    let err = provider
        .complete(&llm_req(), &ctx(), None)
        .await
        .expect_err("backend error");

    mock.assert_async().await;
    assert!(matches!(err, ProviderError::Api { status: 500, .. }));
    let rendered = format!("{err:?}\n{err}");
    assert!(
        !rendered.contains(API_KEY_SECRET),
        "secret leaked: {rendered}"
    );
}

#[tokio::test]
async fn gemini_complete_rejects_empty_candidates() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .with_status(200)
        .with_body(r#"{"candidates": []}"#)
        .expect(1)
        .create_async()
        .await;
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");

    let err = provider
        .complete(&llm_req(), &ctx(), None)
        .await
        .expect_err("malformed response");

    mock.assert_async().await;
    let ProviderError::MalformedResponse(message) = err else {
        panic!("unexpected error");
    };
    assert!(message.contains("candidates"));
}

#[tokio::test]
async fn gemini_complete_cancelled_before_http_call() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .expect(0)
        .create_async()
        .await;
    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");
    let cancel = CancellationToken::new();
    cancel.cancel();

    let err = provider
        .complete(&llm_req(), &ctx(), Some(&cancel))
        .await
        .expect_err("cancelled");

    mock.assert_async().await;
    assert!(matches!(err, ProviderError::Cancelled));
}
