use std::time::Duration;

use crabgent_core::{
    LlmRequest, Provider, ProviderEvent, RunCtx, RunId, StopReason, Subject, ToolDef,
};
use crabgent_provider_google::models::GEMINI_3_5_FLASH;
use crabgent_provider_google::{GoogleConfig, GoogleProvider};
use futures::StreamExt;
use serde_json::json;

const API_KEY_SECRET: &str = "secret-test-google-key-99999";

fn config(base_url: &str) -> GoogleConfig {
    GoogleConfig::new(API_KEY_SECRET.to_owned())
        .with_base_url(base_url.to_owned())
        .with_max_retries(0)
        .with_retry_base_delay(Duration::from_millis(1))
        .with_request_timeout(Duration::from_secs(2))
}

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("test-subject"))
}

fn req() -> LlmRequest {
    LlmRequest {
        model: GEMINI_3_5_FLASH.into(),
        system_prompt: None,
        messages: vec![json!({"role": "user", "content": "hello"})],
        tools: vec![ToolDef {
            name: "lookup".to_owned(),
            description: "look up one thing".to_owned(),
            input_schema: json!({"type": "object"}),
        }],
        max_tokens: None,
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: Some(crabgent_core::ReasoningEffort::High),
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    }
}

#[tokio::test]
async fn stream_generate_content_emits_reasoning_text_tool_usage_and_stop() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock(
            "POST",
            "/v1beta/models/gemini-3.5-flash:streamGenerateContent",
        )
        .match_query(mockito::Matcher::UrlEncoded("alt".into(), "sse".into()))
        .match_header("x-goog-api-key", API_KEY_SECRET)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"thinking\",\"thought\":true}]}}]}\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello\"}]}}]}\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"id\":\"call_1\",\"name\":\"lookup\",\"args\":{\"q\":\"x\"}},\"thoughtSignature\":\"sig\"}]},\"finishReason\":\"MALFORMED_FUNCTION_CALL\"}],\"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":8,\"cachedContentTokenCount\":3}}\n\n",
        ))
        .expect(1)
        .create_async()
        .await;

    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");
    let mut events = provider.stream(&req(), &ctx(), None).await.expect("stream");
    let mut collected = Vec::new();
    while let Some(event) = events.next().await {
        collected.push(event.expect("event"));
    }

    mock.assert_async().await;
    let mut collected = collected.into_iter();
    assert!(matches!(
        collected.next(),
        Some(ProviderEvent::ReasoningDelta(text)) if text == "thinking"
    ));
    assert!(matches!(
        collected.next(),
        Some(ProviderEvent::TextDelta(text)) if text == "hello"
    ));
    assert!(
        matches!(collected.next(), Some(ProviderEvent::ToolUse(call)) if call.id == "call_1" && call.name == "lookup" && call.thought_signature.as_deref() == Some("sig"))
    );
    assert!(
        matches!(collected.next(), Some(ProviderEvent::Usage(usage)) if usage.input_tokens == 5 && usage.output_tokens == 8 && usage.cache_read_tokens == 3)
    );
    assert!(matches!(
        collected.next(),
        Some(ProviderEvent::Stop(StopReason::ToolUse))
    ));
    assert!(collected.next().is_none());
}
