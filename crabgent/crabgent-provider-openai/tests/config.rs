use std::time::Duration;

use crabgent_core::{Provider, ProviderError};
use crabgent_provider_openai::{OpenAiConfig, OpenAiError, OpenAiProvider};
use secrecy::ExposeSecret;

#[test]
fn config_defaults_builders_and_debug_mask_secret() {
    let config = OpenAiConfig::new("secret-test-key-99999")
        .with_max_retries(7)
        .with_retry_base_delay(Duration::from_millis(25))
        .with_request_timeout(Duration::from_secs(3));

    assert_eq!(config.api_key.expose_secret(), "secret-test-key-99999");
    assert_eq!(config.max_retries, 7);
    assert_eq!(config.retry_base_delay, Duration::from_millis(25));
    assert_eq!(config.request_timeout, Duration::from_secs(3));

    let rendered = format!("{config:?}");
    assert!(rendered.contains("****<masked>"));
    assert!(!rendered.contains("secret-test-key-99999"));
}

#[test]
fn provider_constructor_validates_config_and_exposes_state() {
    let http = reqwest::Client::new();
    let config = OpenAiConfig::new("secret-test-key-99999");
    let provider =
        OpenAiProvider::try_from_api_key(http, config.clone()).expect("valid provider config");

    assert_eq!(provider.config().max_retries, config.max_retries);
    assert_eq!(
        provider.auth().wire().endpoint_path(),
        "/v1/chat/completions"
    );
    assert_eq!(provider.name(), "openai");
    assert_eq!(provider.models().len(), 8);
    assert!(provider.capabilities().streaming);
    assert!(provider.capabilities().vision);
    let _http = provider.http_client().clone();

    let empty_key = OpenAiConfig::new("   ");
    let Err(error) = OpenAiProvider::try_from_api_key(reqwest::Client::new(), empty_key) else {
        panic!("empty key should be rejected");
    };
    assert!(format!("{error}").contains("api_key"));

    let zero_timeout =
        OpenAiConfig::new("secret-test-key-99999").with_request_timeout(Duration::from_millis(0));
    let Err(error) = OpenAiProvider::try_from_api_key(reqwest::Client::new(), zero_timeout) else {
        panic!("zero timeout should be rejected");
    };
    assert!(format!("{error}").contains("request_timeout"));
}

#[test]
fn openai_auth_error_maps_to_provider_auth() {
    assert!(matches!(
        ProviderError::from(OpenAiError::Auth),
        ProviderError::Auth(message) if message == "openai authentication failed"
    ));
}

#[test]
fn openai_network_error_maps_to_transport() {
    assert!(matches!(
        ProviderError::from(OpenAiError::Network("offline".to_owned())),
        ProviderError::Transport(message) if message == "offline"
    ));
}

#[test]
fn openai_rate_limit_error_preserves_retry_after() {
    assert!(matches!(
        ProviderError::from(OpenAiError::Api {
            status: 429,
            retry_after_secs: Some(4),
        }),
        ProviderError::RateLimited {
            retry_after_secs: Some(4),
        }
    ));
}

#[test]
fn openai_api_error_preserves_status() {
    assert!(matches!(
        ProviderError::from(OpenAiError::Api {
            status: 500,
            retry_after_secs: None,
        }),
        ProviderError::Api {
            status: 500,
            retry_after_secs: None,
            ..
        }
    ));
}

#[test]
fn openai_decode_error_maps_to_malformed_response() {
    assert!(matches!(
        ProviderError::from(OpenAiError::MalformedResponse("bad json".to_owned())),
        ProviderError::MalformedResponse(message) if message == "bad json"
    ));
}

#[test]
fn openai_config_error_maps_to_other() {
    assert!(matches!(
        ProviderError::from(OpenAiError::ConfigError("bad config".to_owned())),
        ProviderError::Other(message) if message == "bad config"
    ));
}
