use std::time::Duration;

use crabgent_core::{
    LlmRequest, Provider, ProviderError, ProviderEvent, ReasoningEffort, RunCtx, RunId, Subject,
    Usage, WebSearchConfig,
};
use crabgent_provider_openai::{CodexOAuthAuth, OpenAiConfig, OpenAiProvider};
use futures::StreamExt;
use secrecy::SecretString;
use serde_json::json;

/// Live cache test for the Codex OAuth path (Responses wire format).
///
/// Ignored by default because the live backend cache-read signal is
/// nondeterministic. Skips silently when `OPENAI_CODEX_OAUTH_TOKEN_PATH`
/// is unset. Passes both calls through `provider.stream`, accumulates
/// `ProviderEvent::Usage`, and asserts that the second call reports
/// `cache_read_tokens > 0`. Transport and timeout errors also return silently.
#[tokio::test]
#[ignore = "live Codex cache-read signal is nondeterministic"]
async fn openai_codex_caching_real_api() {
    drop(dotenvy::dotenv());

    if std::env::var_os("CARGO_LLVM_COV").is_some() {
        // This is a live backend cache signal, not line-coverage behavior.
        // Keep the workspace coverage gate deterministic when local Codex
        // OAuth credentials are present.
        return;
    }

    let Ok(token_path) = std::env::var("OPENAI_CODEX_OAUTH_TOKEN_PATH") else {
        return;
    };

    let token_text = match std::fs::read_to_string(&token_path) {
        Ok(t) if !t.trim().is_empty() => t,
        _ => return,
    };

    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&token_text) else {
        return;
    };

    let access_token = match parsed.get("access_token").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t.to_owned(),
        _ => return,
    };
    let account_id = parsed
        .get("account_id")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let auth = CodexOAuthAuth::new(SecretString::from(access_token), account_id);
    // api_key is unused by the Codex OAuth path but must be non-empty to
    // pass validate_config.
    let config = OpenAiConfig::new("unused-for-codex")
        .with_max_retries(0)
        .with_request_timeout(Duration::from_secs(30));
    let provider = OpenAiProvider::try_new(reqwest::Client::new(), config, Box::new(auth))
        .expect("valid provider");

    let ctx = RunCtx::new(RunId::new(), Subject::new("cache-test"));
    // Per-run nonce in the session id so call 1 is always cold; both calls
    // in this run still share the same scope so call 2 hits the cache.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    ctx.set_session_id(format!("codex-cache-test-{nonce}"))
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
    "You are a helpful assistant. Be terse. ".repeat(800)
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
    // Drain the full stream rather than breaking on the first `Stop`: a
    // future API revision could trail `Usage` after `Stop` and a premature
    // break would silently zero out `cache_read_tokens`, masking the
    // regression the test is meant to catch.
    while let Some(event) = events.next().await {
        if let ProviderEvent::Usage(u) = event? {
            usage = u;
        }
    }
    Ok(usage)
}
