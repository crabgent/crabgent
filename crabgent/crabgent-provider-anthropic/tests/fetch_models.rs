use std::env;
use std::time::Duration;

use crabgent_core::Provider;
use crabgent_provider_anthropic::{AnthropicConfig, AnthropicProvider, models};
use serde_json::json;

fn provider(endpoint: &str) -> AnthropicProvider {
    let cfg = AnthropicConfig::new("sk-ant-api03-test")
        .with_endpoint(endpoint)
        .with_max_retries(0)
        .with_retry_base_delay(Duration::from_millis(1));
    AnthropicProvider::try_new(reqwest::Client::new(), cfg).expect("valid config")
}

#[tokio::test]
async fn fetch_models_parses_200_response() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "data": [{
            "id": "claude-sonnet-4-6-20251201",
            "display_name": "Claude Sonnet 4.6 (preview)",
            "max_input_tokens": 12_000,
            "max_output_tokens": 42_000
        }]
    });
    let _m = server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body.to_string())
        .create_async()
        .await;

    let provider = provider(&server.url());
    let models = provider.fetch_models().await.expect("fetch");

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id.as_str(), "claude-sonnet-4-6-20251201");
    assert_eq!(models[0].display_name, "Claude Sonnet 4.6 (preview)");
    assert_eq!(models[0].caps.max_input_tokens, 12_000);
    assert_eq!(models[0].caps.max_output_tokens, 42_000);
}

#[tokio::test]
async fn fetch_models_401_returns_model_discovery() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", "/v1/models")
        .with_status(401)
        .with_body("invalid key")
        .create_async()
        .await;

    let provider = provider(&server.url());
    let err = provider.fetch_models().await.expect_err("fetch fails");

    match err {
        crabgent_core::ProviderError::ModelDiscovery { reason } => {
            assert!(reason.contains("401"));
        }
        other => panic!("expected model discovery error, got {other:?}"),
    }
}

#[tokio::test]
async fn new_falls_back_on_model_discovery_500() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", "/v1/models")
        .with_status(500)
        .with_body("temporary outage")
        .create_async()
        .await;

    let cfg = AnthropicConfig::new("sk-ant-api03-test").with_endpoint(server.url());
    let provider = AnthropicProvider::new(cfg)
        .await
        .expect("provider should still construct");

    assert_eq!(provider.models(), models::anthropic_models());
}

#[tokio::test]
async fn fetch_models_discovered_max_input_tokens_returns_specific_value() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "data": [{
            "id": "claude-haiku-4-5-20251001",
            "display_name": "Claude Haiku 4.5 (preview)",
            "max_input_tokens": 12_345,
            "max_output_tokens": 4_000,
        }]
    });
    let _m = server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body.to_string())
        .create_async()
        .await;

    let provider = provider(&server.url());
    let models = provider.fetch_models().await.expect("fetch");
    let model = &models[0];

    assert_eq!(model.id.as_str(), "claude-haiku-4-5-20251001");
    assert_eq!(model.caps.max_input_tokens, 12_345);
    assert_eq!(model.caps.max_output_tokens, 4_000);
}

#[tokio::test]
async fn fetch_models_returns_err_on_timeout() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({ "data": [] }).to_string();
    let _m = server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_chunked_body(move |writer| {
            std::thread::sleep(Duration::from_millis(200));
            std::io::Write::write_all(writer, body.as_bytes())
        })
        .create_async()
        .await;
    let cfg = AnthropicConfig::new("sk-ant-api03-test")
        .with_endpoint(server.url())
        .with_complete_timeout(Duration::from_millis(50));
    let provider = AnthropicProvider::try_new(reqwest::Client::new(), cfg).expect("valid provider");

    let err = provider.fetch_models().await.expect_err("fetch times out");

    assert!(matches!(
        err,
        crabgent_core::ProviderError::ModelDiscovery { .. }
    ));
}

#[tokio::test]
async fn fetch_models_live_call() {
    drop(dotenvy::dotenv());
    let Ok(api_key) = env::var("ANTHROPIC_API_KEY") else {
        return;
    };

    let provider = AnthropicProvider::new(AnthropicConfig::new(api_key))
        .await
        .expect("provider should construct");

    assert!(!provider.models().is_empty());
    assert!(provider.models().len() >= models::anthropic_models().len());
}
