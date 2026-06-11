use std::time::Duration;

use crabgent_core::{
    LlmRequest, Provider, ProviderError, ProviderEvent, ReasoningEffort, RunCtx, RunId, Subject,
    Usage, WebSearchConfig,
};
use crabgent_provider_openai::{OpenAiConfig, OpenAiProvider};
use futures::StreamExt;
use serde_json::json;

/// Live cache test for the `ApiKey` path (Chat Completions wire format).
///
/// Ignored by default because the public Chat Completions cache is a live,
/// non-deterministic backend signal. Run explicitly when validating the
/// `ApiKey` cache path against a real account.
#[tokio::test]
#[ignore = "live OpenAI Chat Completions cache signal is not deterministic"]
async fn openai_apikey_caching_real_api() {
    drop(dotenvy::dotenv());

    let Some(api_key) = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
    else {
        return;
    };

    let config = OpenAiConfig::new(api_key)
        .with_max_retries(0)
        .with_request_timeout(Duration::from_secs(30));
    let provider =
        OpenAiProvider::try_from_api_key(reqwest::Client::new(), config).expect("valid provider");

    let ctx = RunCtx::new(RunId::new(), Subject::new("cache-test"));
    // ApiKey path does not emit per-request cache-scope headers, but the
    // session id is set for symmetry with the Codex test.
    ctx.set_session_id("apikey-cache-test")
        .expect("session id written once");

    let instructions = build_instructions();
    let req = build_request(instructions);

    let r1 = match stream_usage(&provider, &req, &ctx).await {
        Ok(u) => u,
        Err(ProviderError::Transport(_) | ProviderError::Timeout) => return,
        Err(other) => panic!("unexpected provider error on call 1: {other}"),
    };

    tokio::time::sleep(Duration::from_secs(2)).await;

    let r2 = match stream_usage(&provider, &req, &ctx).await {
        Ok(u) => u,
        Err(ProviderError::Transport(_) | ProviderError::Timeout) => return,
        Err(other) => panic!("unexpected provider error on call 2: {other}"),
    };

    assert_eq!(r1.cache_read_tokens, 0, "call 1 must not read from cache");
    assert!(
        r2.cache_read_tokens > 0,
        "call 2 must read from cache (got {})",
        r2.cache_read_tokens,
    );
}

fn build_instructions() -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    // Nonce in the PREFIX so the byte-stable cache key differs across runs.
    // Auto-cache fires on the prefix bytes regardless of `prompt_cache_key`,
    // so a suffix nonce alone would still let call 1 of run N+1 hit the
    // prefix that was primed by run N. Both calls within this run share the
    // same prefix so call 2 hits the cache.
    format!(
        "Cache-test nonce: {nonce}. {}",
        "You are a helpful assistant. Be terse. ".repeat(800),
    )
}

fn build_request(instructions: String) -> LlmRequest {
    LlmRequest {
        model: "gpt-5.5".into(),
        system_prompt: Some(instructions),
        messages: vec![json!({
            "role": "user",
            "content": [{"type": "text", "text": "hello"}],
        })],
        tools: vec![],
        max_tokens: None,
        temperature: None,
        stop_sequences: vec![],
        // Kernel would populate this from model caps; bypass run loop by
        // setting explicitly to match production wire shape.
        reasoning_effort: Some(ReasoningEffort::Low),
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    }
}

async fn stream_usage(
    provider: &OpenAiProvider,
    req: &LlmRequest,
    ctx: &RunCtx,
) -> Result<Usage, ProviderError> {
    let mut events = provider.stream(req, ctx, None).await?;
    let mut usage = Usage::default();
    // Drain the full stream rather than breaking on the first `Stop`: a future
    // API revision could trail `Usage` after `Stop` and a premature break
    // would silently zero out cache_read_tokens, masking the regression the
    // test is meant to catch.
    while let Some(event) = events.next().await {
        if let ProviderEvent::Usage(u) = event? {
            usage = u;
        }
    }
    Ok(usage)
}
