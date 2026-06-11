use std::time::Duration;

use crabgent_core::{LlmRequest, RunCtx, RunId, Subject, ToolDef};
use crabgent_provider_google::GoogleConfig;
use crabgent_provider_google::models::GEMINI_3_5_FLASH;
use serde_json::{Value, json};

pub const API_KEY_SECRET: &str = "secret-test-google-key-99999";

pub fn config(base_url: &str) -> GoogleConfig {
    GoogleConfig::new(API_KEY_SECRET.to_owned())
        .with_base_url(base_url.to_owned())
        .with_max_retries(0)
        .with_retry_base_delay(Duration::from_millis(1))
        .with_request_timeout(Duration::from_secs(2))
}

pub fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("test-subject"))
}

pub fn llm_req() -> LlmRequest {
    LlmRequest {
        model: GEMINI_3_5_FLASH.into(),
        system_prompt: Some("be terse".to_owned()),
        messages: vec![json!({
            "role": "user",
            "content": [{"type": "text", "text": "call a tool"}],
        })],
        tools: vec![ToolDef {
            name: "lookup".to_owned(),
            description: "look up one thing".to_owned(),
            input_schema: json!({"type": "object"}),
        }],
        max_tokens: Some(64),
        temperature: Some(0.5),
        stop_sequences: vec!["STOP".to_owned()],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    }
}

pub fn assert_llm_body(request: &mockito::Request) -> bool {
    let Some(value) = body_json(request) else {
        return false;
    };
    value["systemInstruction"]["parts"][0]["text"] == "be terse"
        && value["contents"][0]["parts"][0]["text"] == "call a tool"
        && value["generationConfig"]["maxOutputTokens"] == 64
        && value["generationConfig"]["temperature"] == 0.5
        && value["generationConfig"]["stopSequences"][0] == "STOP"
        && value["tools"][0]["functionDeclarations"][0]["name"] == "lookup"
}

pub fn assert_cache_create_body(request: &mockito::Request) -> bool {
    let Some(value) = body_json(request) else {
        return false;
    };
    value.get("contents").is_none()
        && value.pointer("/model") == Some(&json!("models/gemini-3.5-flash"))
        && value.pointer("/ttl") == Some(&json!("3600s"))
        && value.pointer("/systemInstruction/parts/0/text") == Some(&json!("be terse"))
        && value.pointer("/tools/0/functionDeclarations/0/name") == Some(&json!("lookup"))
}

pub fn assert_cached_generate_body(request: &mockito::Request) -> bool {
    let Some(value) = body_json(request) else {
        return false;
    };
    value.get("systemInstruction").is_none()
        && value.get("tools").is_none()
        && value.get("toolConfig").is_none()
        && value.pointer("/cachedContent") == Some(&json!("cachedContents/test-cache"))
        && value.pointer("/contents/0/parts/0/text") == Some(&json!("call a tool"))
        && value.pointer("/generationConfig/maxOutputTokens") == Some(&json!(64))
}

pub fn assert_multimodal_body(request: &mockito::Request) -> bool {
    let Some(value) = body_json(request) else {
        return false;
    };
    value.get("systemInstruction").is_none()
        && value.get("generationConfig").is_none()
        && value.get("tools").is_none()
        && value["contents"]
            .as_array()
            .is_some_and(|contents| contents.len() == 4)
        && value["contents"][0]["parts"][0]["inlineData"]["mimeType"] == "image/png"
        && value["contents"][0]["parts"][0]["inlineData"]["data"] == "encoded-image"
        && value["contents"][1]["role"] == "model"
        && value["contents"][1]["parts"][0]["text"] == "previous"
        && value["contents"][1]["parts"][1]["functionCall"]["id"] == "call_lookup_old"
        && value["contents"][1]["parts"][1]["functionCall"]["name"] == "lookup"
        && value["contents"][1]["parts"][1]["thoughtSignature"] == "signature-old"
        && value["contents"][2]["parts"][0]["functionResponse"]["id"] == "call_lookup_old"
        && value["contents"][2]["parts"][0]["functionResponse"]["name"] == "lookup"
        && value["contents"][2]["parts"][0]["functionResponse"]["response"]["answer"] == 42
        && value["contents"][3]["parts"][0]["text"] == "plain text"
}

fn body_json(request: &mockito::Request) -> Option<Value> {
    let body = request.utf8_lossy_body().ok()?;
    serde_json::from_str::<Value>(&body).ok()
}
