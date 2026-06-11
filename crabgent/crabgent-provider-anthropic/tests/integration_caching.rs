use crabgent_core::{LlmRequest, ProviderError};
use crabgent_provider_anthropic::{AnthropicClient, AnthropicConfig};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn anthropic_caching_real_api() {
    drop(dotenvy::dotenv());
    let Some(key) = load_api_key() else {
        return;
    };

    let config = AnthropicConfig::new(key)
        .with_cache_ttl(Some("5m".to_string()))
        .expect("valid cache_ttl");
    let client = AnthropicClient::try_new(reqwest::Client::new(), config).expect("client");
    let system = build_long_system();

    let req1 = request(system.clone(), "Reply with the word cached.");
    let Some(r1) = live_complete(&client, &req1)
        .await
        .expect("live anthropic caching call should succeed")
    else {
        return;
    };
    assert!(
        r1.usage.cache_creation_tokens > 0,
        "expected cache_creation>0 on run 1"
    );

    tokio::time::sleep(Duration::from_secs(2)).await;

    let req2 = request(system, "Reply with the word reused.");
    let Some(r2) = live_complete(&client, &req2)
        .await
        .expect("live anthropic caching call should succeed")
    else {
        return;
    };
    assert!(
        r2.usage.cache_read_tokens > 0,
        "expected cache_read>0 on run 2"
    );
}

fn load_api_key() -> Option<String> {
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
}

fn request(system: String, user_text: &str) -> LlmRequest {
    LlmRequest {
        model: "claude-haiku-4-5".into(),
        system_prompt: Some(system),
        messages: vec![json!({"role": "user", "content": [{"type": "text", "text": user_text}]})],
        tools: vec![],
        max_tokens: Some(16),
        temperature: Some(0.0),
        stop_sequences: vec![],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    }
}

fn build_long_system() -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!(
        "{}Unique cache test marker: {nonce}.",
        "You are a helpful assistant. ".repeat(800),
    )
}

async fn live_complete(
    client: &AnthropicClient,
    req: &LlmRequest,
) -> Result<Option<crabgent_core::LlmResponse>, ProviderError> {
    match client.call_complete(req, None).await {
        Ok(response) => Ok(Some(response)),
        Err(ProviderError::Transport(_) | ProviderError::Timeout) => Ok(None),
        Err(error) => Err(error),
    }
}
