use mockito::Matcher;
use std::time::Duration;

use crabgent_core::{Provider, ProviderError};
use crabgent_provider_openai::{
    ApiKeyAuth, AuthStrategy, CodexOAuthAuth, OpenAiConfig, OpenAiProvider,
    models::{PROVIDER, discovered_model, openai_models},
};
use secrecy::SecretString;
use serde_json::json;

const API_KEY_SECRET: &str = "secret-test-key-99999";
const CODEX_TOKEN_SECRET: &str = "secret-test-token-99999";
const CODEX_ACCOUNT_ID: &str = "account-test-id";

fn config(max_retries: u32) -> OpenAiConfig {
    OpenAiConfig::new(API_KEY_SECRET)
        .with_max_retries(max_retries)
        .with_retry_base_delay(Duration::from_millis(1))
        .with_request_timeout(Duration::from_secs(2))
}

fn api_key_auth(base_url: &str) -> ApiKeyAuth {
    ApiKeyAuth::new(SecretString::from(API_KEY_SECRET.to_owned()))
        .with_base_url(base_url.to_owned())
}

fn codex_auth(base_url: &str) -> CodexOAuthAuth {
    CodexOAuthAuth::new(
        SecretString::from(CODEX_TOKEN_SECRET.to_owned()),
        Some(CODEX_ACCOUNT_ID.to_owned()),
    )
    .with_base_url(base_url.to_owned())
}

#[test]
fn apikey_auth_default_supports_discovery_true() {
    let auth = api_key_auth("https://api.openai.com");
    assert!(auth.supports_model_discovery());
}

#[test]
fn codex_oauth_override_supports_discovery_false() {
    let auth = codex_auth("https://chatgpt.com");
    assert!(!auth.supports_model_discovery());
}

#[tokio::test]
async fn codex_oauth_skips_model_discovery_no_http_call() {
    let mut server = mockito::Server::new_async().await;
    let no_call_mock = server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_body(json!({"data": []}).to_string())
        .expect(0)
        .create_async()
        .await;

    let provider = OpenAiProvider::new(config(0), Box::new(codex_auth(&server.url())))
        .await
        .expect("provider should build with fallback models");
    assert_eq!(provider.name(), PROVIDER);
    assert_eq!(provider.models(), openai_models());
    assert_eq!(
        provider.fetch_models().await.expect("fetch ok").len(),
        provider.models().len()
    );
    no_call_mock.assert_async().await;
}

#[tokio::test]
async fn apikey_fetch_models_calls_v1_models_endpoint() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "object": "list",
        "data": [
            {"id": "gpt-5", "object": "model", "owned_by":"openai", "created":1},
            {"id": "text-babbage", "object": "model", "owned_by":"openai", "created":1},
            {"id": "gpt-5-codex", "object": "model", "owned_by":"openai", "created":1}
        ]
    });
    let mock = server
        .mock("GET", "/v1/models")
        .match_header(
            "authorization",
            Matcher::Exact(format!("Bearer {API_KEY_SECRET}")),
        )
        .expect(1)
        .with_status(200)
        .with_body(body.to_string())
        .create_async()
        .await;

    let provider = OpenAiProvider::new(config(0), Box::new(api_key_auth(&server.url())))
        .await
        .expect("provider should build");
    let models = provider.models();
    let model_ids = models
        .iter()
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    mock.assert_async().await;
    assert!(model_ids.iter().any(|id| id.as_str() == "gpt-5"));
    assert!(model_ids.iter().any(|id| id.as_str() == "gpt-5-codex"));
    for model in models {
        assert!(!model.caps.supports_audio);
    }
}

#[tokio::test]
async fn apikey_fetch_models_401_falls_back_in_constructor() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/v1/models")
        .with_status(401)
        .with_body("unauthorized")
        .expect(1)
        .create_async()
        .await;

    let provider = OpenAiProvider::new(config(1), Box::new(api_key_auth(&server.url())))
        .await
        .expect("fallback to static catalog");
    let mut expected = openai_models()
        .into_iter()
        .map(|model| model.id.as_str().to_owned())
        .collect::<Vec<_>>();
    let mut actual = provider
        .models()
        .into_iter()
        .map(|model| model.id.as_str().to_owned())
        .collect::<Vec<_>>();
    expected.sort_unstable();
    actual.sort_unstable();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn apikey_fetch_models_401_no_key_leak() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/v1/models")
        .with_status(401)
        .with_body(format!("bad key {API_KEY_SECRET}"))
        .expect(1)
        .create_async()
        .await;
    let provider = OpenAiProvider::try_new(
        reqwest::Client::new(),
        config(0),
        Box::new(api_key_auth(&server.url())),
    )
    .expect("test provider should be valid");

    let err = provider.fetch_models().await.expect_err("auth rejected");
    let rendered = format!("{err:?}\n{err}");

    assert!(matches!(err, ProviderError::Auth(_)));
    assert!(
        !rendered.contains(API_KEY_SECRET),
        "secret leaked: {rendered}"
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn apikey_fetch_models_discovered_ids_returns_specific_set() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_body(
            json!({
                "object":"list",
                "data": [
                    {"id":"gpt-1", "object":"model", "created":1, "owned_by":"openai"},
                    {"id":"text-curie", "object":"model", "created":2, "owned_by":"openai"},
                    {"id":"gpt-2", "object":"model", "created":3, "owned_by":"openai"},
                ]
            })
            .to_string(),
        )
        .create_async()
        .await;

    let provider = OpenAiProvider::try_new(
        reqwest::Client::new(),
        config(0),
        Box::new(api_key_auth(&server.url())),
    )
    .expect("test provider should be valid");
    let models = provider.fetch_models().await.expect("fetch ok");
    let ids = models.into_iter().map(|model| model.id).collect::<Vec<_>>();
    let expected = vec![discovered_model("gpt-1").id, discovered_model("gpt-2").id];
    assert_eq!(ids, expected);
}

#[tokio::test]
async fn apikey_fetch_models_known_ids_use_catalog_capabilities() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_body(
            json!({
                "object":"list",
                "data": [
                    {"id":"gpt-5.5", "object":"model", "created":1, "owned_by":"openai"},
                    {"id":"gpt-future", "object":"model", "created":2, "owned_by":"openai"}
                ]
            })
            .to_string(),
        )
        .create_async()
        .await;

    let provider = OpenAiProvider::try_new(
        reqwest::Client::new(),
        config(0),
        Box::new(api_key_auth(&server.url())),
    )
    .expect("test provider should be valid");

    let models = provider.fetch_models().await.expect("fetch ok");
    let known = models
        .iter()
        .find(|model| model.id.as_str() == "gpt-5.5")
        .expect("known catalog model should be discovered");
    assert!(known.caps.supports_vision);
    assert!(known.caps.supports_tools);
    assert!(known.caps.supports_prompt_cache);
    assert!(known.caps.supports_thinking);
    assert!(!known.caps.supports_audio);
    assert!(known.pricing.is_some());
}

#[tokio::test]
async fn apikey_fetch_models_unknown_ids_are_conservative() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_body(
            json!({
                "object":"list",
                "data": [
                    {"id":"gpt-future", "object":"model", "created":1, "owned_by":"openai"}
                ]
            })
            .to_string(),
        )
        .create_async()
        .await;

    let provider = OpenAiProvider::try_new(
        reqwest::Client::new(),
        config(0),
        Box::new(api_key_auth(&server.url())),
    )
    .expect("test provider should be valid");

    let models = provider.fetch_models().await.expect("fetch ok");
    let unknown = models
        .iter()
        .find(|model| model.id.as_str() == "gpt-future")
        .expect("unknown model should be discovered");
    assert!(!unknown.caps.supports_vision);
    assert!(!unknown.caps.supports_tools);
    assert!(!unknown.caps.supports_prompt_cache);
    assert!(!unknown.caps.supports_thinking);
    assert!(!unknown.caps.supports_audio);
    assert_eq!(unknown.caps.reasoning_effort, None);
    assert!(unknown.pricing.is_none());
}

#[tokio::test]
async fn openai_fetch_models_integration() {
    drop(dotenvy::dotenv());
    let Ok(api_key) = std::env::var("OPENAI_API_KEY") else {
        return;
    };

    let config = OpenAiConfig::new(api_key.clone())
        .with_max_retries(0)
        .with_request_timeout(Duration::from_secs(30));
    let provider = OpenAiProvider::try_new(
        reqwest::Client::new(),
        config,
        Box::new(ApiKeyAuth::new(SecretString::from(api_key))),
    )
    .expect("provider should be usable with env key");
    let models = match provider.fetch_models().await {
        Ok(models) => models,
        Err(ProviderError::Transport(_) | ProviderError::Timeout) => return,
        Err(other) => panic!("unexpected model discovery error: {other}"),
    };

    assert!(!models.is_empty());
    assert!(
        models
            .iter()
            .any(|model| model.id.as_str().starts_with("gpt-"))
    );
}
