use std::sync::LazyLock;

use serde_json::{Value, json};

use crate::session::McpSessionEntry;
use crate::{McpServer, McpServerError};

pub const CHAT_TOOL_NAME: &str = "chat";

pub static CHAT_INPUT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "message": { "type": "string" },
        },
        "required": ["message"],
    })
});

pub static CHAT_OUTPUT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "reply_text": { "type": "string" },
            "session_id": { "type": "string" },
            "transcript_delta": { "type": "array" },
        },
        "required": ["reply_text", "session_id", "transcript_delta"],
    })
});

pub async fn handle_chat(
    server: &McpServer,
    entry: &McpSessionEntry,
    args: Value,
) -> Result<Value, McpServerError> {
    let message_text = args
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| McpServerError::InvalidParams("message is required".into()))?
        .to_owned();
    let token = entry.cancel_token.clone();
    let request = crabgent_core::RunRequest {
        pause: None,
        run_id: crabgent_core::RunId::new(),
        subject: entry.subject.clone(),
        model: server.config().default_model.clone(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: vec![],
        messages: vec![crabgent_core::Message::User {
            content: vec![crabgent_core::ContentBlock::Text { text: message_text }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: None,
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
    };
    let reply_text = server
        .kernel()
        .run(request, Some(&token))
        .await
        .map_err(McpServerError::KernelRun)?;

    Ok(json!({
        "content": [{
            "type": "text",
            "text": reply_text,
        }],
        "reply_text": reply_text,
        "session_id": entry.id.to_string(),
        "transcript_delta": [],
    }))
}
