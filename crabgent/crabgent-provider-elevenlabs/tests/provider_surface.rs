mod common;

use std::sync::Arc;

use crabgent_core::{SttProvider, SttProviderCapabilities};
use crabgent_provider_elevenlabs::{
    ElevenLabsConfig, ElevenLabsModelId, ElevenLabsSttProvider, SttWsClient,
};

use crate::common::stt_test_ctx;

#[tokio::test]
async fn config_debug_masks_api_key_and_keeps_api_base() {
    let config =
        ElevenLabsConfig::new("secret-test-xi-key-99999").with_api_base("https://example.test/");

    let rendered = format!("{config:?}");

    assert!(!rendered.contains("secret-test-xi-key-99999"));
    assert!(rendered.contains("****<masked>"));
    assert_eq!(config.api_base(), "https://example.test/");
}

#[test]
fn model_id_wraps_and_converts_to_stt_model_id() {
    let model = ElevenLabsModelId::new("scribe_v2");

    assert_eq!(model.as_stt_model_id().as_str(), "scribe_v2");
    let stt_model: crabgent_core::SttModelId = model.into();
    assert_eq!(stt_model.as_str(), "scribe_v2");
}

#[tokio::test]
async fn provider_exposes_config_capabilities_and_fallback_models() {
    let ctx = stt_test_ctx().await;
    let ws_client: Arc<dyn SttWsClient> = ctx.ws_client.clone();
    let provider = ElevenLabsSttProvider::try_new(
        reqwest::Client::new(),
        Arc::new(ctx.config.clone()),
        ws_client,
    )
    .expect("valid STT provider");

    assert_eq!(provider.config().api_base(), ctx.config.api_base());
    assert_eq!(
        provider.capabilities(),
        SttProviderCapabilities {
            streaming: true,
            audio: true,
        }
    );
    let ids = provider
        .models()
        .into_iter()
        .map(|model| model.id.to_string())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"scribe_v2".to_owned()));
    assert!(ids.contains(&"scribe_v1".to_owned()));
    assert!(ids.contains(&"scribe_v2_realtime".to_owned()));
}

#[tokio::test]
async fn invalid_config_is_rejected() {
    let ctx = stt_test_ctx().await;
    let ws_client: Arc<dyn SttWsClient> = ctx.ws_client;

    let Err(empty_key) = ElevenLabsSttProvider::try_new(
        reqwest::Client::new(),
        Arc::new(ElevenLabsConfig::new("   ")),
        Arc::clone(&ws_client),
    ) else {
        panic!("empty key should be rejected");
    };
    assert!(format!("{empty_key}").contains("api_key must not be empty"));

    let Err(empty_base) = ElevenLabsSttProvider::try_new(
        reqwest::Client::new(),
        Arc::new(ElevenLabsConfig::new("secret-test-xi-key-99999").with_api_base("")),
        ws_client,
    ) else {
        panic!("empty api_base should be rejected");
    };
    assert!(format!("{empty_base}").contains("api_base must not be empty"));
}
