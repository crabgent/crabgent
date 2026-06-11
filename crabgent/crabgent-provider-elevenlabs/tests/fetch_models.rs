mod common;

use crabgent_core::{SttError, SttProvider};
use crabgent_provider_elevenlabs::{ElevenLabsConfig, ElevenLabsSttProvider};
use mockito::Matcher;
use serde_json::json;

const XI_API_KEY: &str = "secret-test-xi-key-99999";

#[tokio::test]
async fn elevenlabs_fetch_models_parses_response_filters_tts() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/v1/models")
        .match_header("xi-api-key", Matcher::Exact(XI_API_KEY.to_owned()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!([
                {
                    "model_id": "scribe_v2",
                    "name": "Scribe v2",
                    "can_do_text_to_speech": false
                },
                {
                    "model_id": "eleven_multilingual_v2",
                    "name": "TTS",
                    "can_do_text_to_speech": true
                }
            ])
            .to_string(),
        )
        .expect(1)
        .create_async()
        .await;
    let provider = provider(server.url());

    let models = provider.fetch_models().await.expect("models");

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id.as_str(), "scribe_v2");
    assert!(!models[0].supports_streaming);
    assert!(models[0].supports_diarization);
    mock.assert_async().await;
}

#[tokio::test]
async fn elevenlabs_fetch_models_401_falls_back() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/v1/models")
        .with_status(401)
        .with_body(format!("bad key {XI_API_KEY}"))
        .expect(1)
        .create_async()
        .await;

    let provider = ElevenLabsSttProvider::new(config(server.url()))
        .await
        .expect("provider");

    let models = provider.models();
    assert!(models.iter().any(|model| model.id.as_str() == "scribe_v2"));
    assert!(
        models
            .iter()
            .any(|model| model.id.as_str() == "scribe_v2_realtime")
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn elevenlabs_fetch_models_401_error_is_redacted() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/v1/models")
        .with_status(401)
        .with_body(format!("bad key {XI_API_KEY}"))
        .expect(1)
        .create_async()
        .await;
    let provider = provider(server.url());

    let err = provider.fetch_models().await.expect_err("auth rejected");
    let rendered = format!("{err:?}\n{err}");

    assert!(matches!(err, SttError::ModelDiscovery { .. }));
    assert!(!rendered.contains(XI_API_KEY), "secret leaked: {rendered}");
    mock.assert_async().await;
}

#[tokio::test]
async fn elevenlabs_fetch_models_integration() {
    drop(dotenvy::dotenv());
    let Ok(api_key) = std::env::var("ELEVENLABS_API_KEY") else {
        return;
    };
    let provider = ElevenLabsSttProvider::new(ElevenLabsConfig::new(api_key))
        .await
        .expect("provider");

    let models = match provider.fetch_models().await {
        Ok(models) => models,
        Err(SttError::ModelDiscovery { reason })
            if reason == "elevenlabs model discovery network error" =>
        {
            return;
        }
        Err(error) => panic!("elevenlabs live model discovery failed: {error:?}"),
    };

    assert!(!models.is_empty());
}

fn provider(api_base: String) -> ElevenLabsSttProvider {
    ElevenLabsSttProvider::try_from_api_key(reqwest::Client::new(), config(api_base))
        .expect("valid provider")
}

fn config(api_base: String) -> ElevenLabsConfig {
    ElevenLabsConfig::new(XI_API_KEY).with_api_base(api_base)
}
