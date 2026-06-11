use crabgent_core::{ModelId, ModelRegistry, Provider};
use crabgent_provider_openai::models::{GPT_5_3_CODEX, GPT_5_3_CODEX_SPARK, GPT_5_5, PROVIDER};
use crabgent_provider_openai::{ApiKeyAuth, CodexOAuthAuth, OpenAiConfig, OpenAiProvider};
use secrecy::SecretString;

#[test]
fn provider_capabilities() {
    let api_key = SecretString::from("secret-test-key-99999".to_owned());
    let config = OpenAiConfig::new(api_key.clone()).with_max_retries(0);
    let provider = OpenAiProvider::try_new(
        reqwest::Client::new(),
        config,
        Box::new(ApiKeyAuth::new(api_key)),
    )
    .expect("test config should be valid");

    let caps = provider.capabilities();
    assert!(caps.vision);
    assert!(caps.tools);
    assert!(caps.prompt_cache);
    assert!(caps.streaming);
    assert!(caps.thinking);
    // Provider-level audio is on; only gpt-4o-audio-preview clears the
    // per-model gate, chat-only models keep supports_audio: false.
    assert!(caps.audio);
    assert!(!caps.web_search);

    let mut registry = ModelRegistry::new();
    for model in provider.models() {
        assert_eq!(model.provider, PROVIDER);
        registry.insert(model).expect("unique OpenAI model id");
    }

    assert_model_caps(&registry, GPT_5_5);
    assert_model_caps(&registry, GPT_5_3_CODEX);
    assert_model_caps(&registry, GPT_5_3_CODEX_SPARK);
}

fn assert_model_caps(registry: &ModelRegistry, id: &str) {
    let model = registry
        .get(&ModelId::new(id))
        .expect("registered OpenAI model");
    assert!(model.caps.supports_vision);
    assert!(model.caps.supports_tools);
    assert!(model.caps.supports_prompt_cache);
    assert!(model.caps.supports_thinking);
    assert!(!model.caps.supports_audio);
}

#[test]
fn apikey_provider_does_not_advertise_web_search() {
    let api_key = SecretString::from("secret-test-key-99999".to_owned());
    let config = OpenAiConfig::new(api_key.clone()).with_max_retries(0);
    let provider = OpenAiProvider::try_new(
        reqwest::Client::new(),
        config,
        Box::new(ApiKeyAuth::new(api_key)),
    )
    .expect("test config should be valid");

    assert!(!provider.capabilities().web_search);
}

#[test]
fn codex_provider_advertises_web_search() {
    let api_key = SecretString::from("secret-test-key-99999".to_owned());
    let token = SecretString::from("codex-test-token".to_owned());
    let config = OpenAiConfig::new(api_key).with_max_retries(0);
    let provider = OpenAiProvider::try_new(
        reqwest::Client::new(),
        config,
        Box::new(CodexOAuthAuth::new(token, None)),
    )
    .expect("test config should be valid");

    assert!(provider.capabilities().web_search);
}

#[test]
fn web_search_model_flags() {
    use crabgent_provider_openai::models::{
        GPT_5_2, GPT_5_3_CODEX, GPT_5_3_CODEX_SPARK, GPT_5_4, GPT_5_4_MINI, GPT_5_5, openai_models,
    };

    let catalog: std::collections::HashMap<_, _> = openai_models()
        .into_iter()
        .map(|m| (m.id.as_str().to_owned(), m))
        .collect();

    // Models that support hosted web search
    for id in [GPT_5_5, GPT_5_4, GPT_5_2] {
        assert!(
            catalog[id].caps.supports_web_search,
            "{id} must have supports_web_search=true"
        );
    }

    // Models that do not support hosted web search (conservative)
    for id in [GPT_5_3_CODEX, GPT_5_3_CODEX_SPARK, GPT_5_4_MINI] {
        assert!(
            !catalog[id].caps.supports_web_search,
            "{id} must have supports_web_search=false"
        );
    }
}
