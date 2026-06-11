//! Build the Anthropic Messages API request body from a kernel
//! `LlmRequest`. Translates crabgent's typed message format into the
//! content-block shape Anthropic expects: assistant turns become
//! `{role: "assistant", content: [text, tool_use, ...]}` and tool
//! results become `{role: "user", content: [{type: "tool_result", ...}]}`.

use crabgent_core::{
    LlmRequest, ProjectedContent, ProjectedToolCall, ProjectedTurn, ToolChoice,
    project_conversation,
};
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::caching;

const DEFAULT_MAX_TOKENS: u32 = 4096;
const ANTHROPIC_BLOCKED_SCHEMA_KEYWORDS: [&str; 3] = ["allOf", "oneOf", "anyOf"];

/// Errors that can occur when building an Anthropic request body.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum RequestBuildError {
    /// Anthropic rejects requests that specify both `allowed_domains` and
    /// `blocked_domains` for web search.
    #[error(
        "web_search: allowed_domains and blocked_domains are mutually exclusive; \
         supply at most one"
    )]
    WebSearchDomainConflict,
}

/// Build the Anthropic Messages API body. Sets `stream` to the value the
/// caller passes; the streaming endpoint expects `true`, complete uses
/// `false`.
pub fn build_body(
    req: &LlmRequest,
    stream: bool,
    cache_ttl: Option<&str>,
    model_id: &str,
) -> Result<Value, RequestBuildError> {
    let mut messages = transform_messages(&req.messages);
    caching::apply_message_cache(&mut messages, cache_ttl, model_id);

    let mut body = Map::new();
    body.insert("model".to_owned(), json!(req.model));
    body.insert(
        "max_tokens".to_owned(),
        json!(req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
    );
    body.insert("stream".to_owned(), Value::Bool(stream));
    body.insert("messages".to_owned(), Value::Array(messages));
    let system = caching::wrap_system(req.system_prompt.as_deref(), cache_ttl);
    if !system.is_null() {
        body.insert("system".to_owned(), system);
    }
    let mut tools = tools_to_anthropic(req)?;
    caching::apply_tool_cache(&mut tools, cache_ttl);
    if !tools.is_empty() {
        body.insert("tools".to_owned(), Value::Array(tools));
        // tool_choice is only meaningful when tools are present; the empty
        // case above leaves it off so Anthropic does not reject the request.
        if let Some(tc) = &req.tool_choice {
            body.insert("tool_choice".to_owned(), tool_choice_to_anthropic(tc));
        }
    }
    if let Some(t) = req.temperature {
        body.insert("temperature".to_owned(), json!(t));
    }
    if !req.stop_sequences.is_empty() {
        body.insert("stop_sequences".to_owned(), json!(req.stop_sequences));
    }
    Ok(Value::Object(body))
}

fn transform_messages(messages: &[Value]) -> Vec<Value> {
    project_conversation(messages)
        .iter()
        .filter_map(transform_turn)
        .collect()
}

fn transform_turn(turn: &ProjectedTurn) -> Option<Value> {
    match turn {
        ProjectedTurn::User { content, raw } => transform_user(content.as_ref(), raw),
        ProjectedTurn::Assistant { text, tool_calls } => {
            Some(transform_assistant(text, tool_calls))
        }
        ProjectedTurn::ToolResult {
            call_id,
            output,
            is_error,
        } => Some(transform_tool_result(call_id, output, *is_error)),
        ProjectedTurn::Unknown {
            role: Some(_), raw, ..
        } => Some(raw.clone()),
        ProjectedTurn::ProviderBlock { provider, block } => {
            // Only echo blocks that originated from Anthropic. Blocks from
            // other providers (OpenAI, Google) are incompatible wire formats
            // and must be skipped so encrypted_content correlation stays
            // consistent.
            if provider == "anthropic" {
                // Emit as a user-role message with the block in content[]
                // so Anthropic can correlate encrypted_content across turns.
                Some(json!({
                    "role": "user",
                    "content": [block]
                }))
            } else {
                None
            }
        }
        // System, ChannelOutbound, Unknown { role: None }, and future
        // non-exhaustive variants are all no-ops for Anthropic wire.
        _ => None,
    }
}

fn transform_user(content: Option<&ProjectedContent>, raw: &Value) -> Option<Value> {
    let content = content?;
    let transformed_content = if let ProjectedContent::Blocks(blocks) = content {
        let blocks: Vec<Value> = blocks.iter().filter_map(transform_user_block).collect();
        if blocks.is_empty() {
            return None;
        }
        Value::Array(blocks)
    } else if let ProjectedContent::Raw(content) = content {
        content.clone()
    } else {
        return None;
    };

    let mut transformed = Map::new();
    transformed.insert("role".to_owned(), json!("user"));
    transformed.insert("content".to_owned(), transformed_content);
    if let Some(cache_control) = raw.get("cache_control") {
        transformed.insert("cache_control".to_owned(), cache_control.clone());
    }
    Some(Value::Object(transformed))
}

fn transform_user_block(block: &ProjectedContent) -> Option<Value> {
    match block {
        ProjectedContent::Image { mime, data, .. } => Some(transform_user_image_block(mime, data)),
        ProjectedContent::Other(raw) if is_malformed_image_block(raw) => None,
        ProjectedContent::Text { raw, .. } | ProjectedContent::Other(raw) => Some(raw.clone()),
        ProjectedContent::Raw(_) | ProjectedContent::Blocks(_) => None,
    }
}

fn transform_user_image_block(mime: &str, data: &str) -> Value {
    json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": mime,
            "data": data,
        }
    })
}

fn transform_assistant(text: &str, tool_calls: &[ProjectedToolCall]) -> Value {
    let mut content: Vec<Value> = Vec::new();
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    for call in tool_calls {
        content.push(json!({
            "type": "tool_use",
            "id": call.id,
            "name": call.name.as_deref().unwrap_or(""),
            "input": call.args,
        }));
    }
    json!({"role": "assistant", "content": content})
}

fn transform_tool_result(call_id: &str, output: &Value, is_error: bool) -> Value {
    let content_str = match output {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    let block = json!({
        "type": "tool_result",
        "tool_use_id": call_id,
        "content": content_str,
        "is_error": is_error,
    });
    json!({"role": "user", "content": [block]})
}

fn is_malformed_image_block(raw: &Value) -> bool {
    raw.get("type").and_then(Value::as_str) == Some("image")
}

fn tools_to_anthropic(req: &LlmRequest) -> Result<Vec<Value>, RequestBuildError> {
    let ws = &req.web_search;
    if ws.enabled && !ws.allowed_domains.is_empty() && !ws.blocked_domains.is_empty() {
        return Err(RequestBuildError::WebSearchDomainConflict);
    }

    let mut out: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": sanitize_schema(&t.input_schema),
            })
        })
        .collect();

    if ws.enabled {
        out.push(build_web_search_tool(ws));
    }

    Ok(out)
}

fn build_web_search_tool(ws: &crabgent_core::types::WebSearchConfig) -> Value {
    let mut tool = json!({
        "type": "web_search_20250305",
        "name": "web_search",
    });
    let obj = tool
        .as_object_mut()
        .expect("invariant: json! produces object");
    if let Some(max_uses) = ws.max_uses {
        obj.insert("max_uses".to_owned(), json!(max_uses));
    }
    if !ws.allowed_domains.is_empty() {
        obj.insert("allowed_domains".to_owned(), json!(ws.allowed_domains));
    }
    if !ws.blocked_domains.is_empty() {
        obj.insert("blocked_domains".to_owned(), json!(ws.blocked_domains));
    }
    tool
}

/// Translate the neutral `ToolChoice` into Anthropic's wire shape. The
/// enum's own serde form is the persistence shape; the wire form comes
/// from these explicit arms, never from serializing the enum directly.
fn tool_choice_to_anthropic(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({"type": "auto"}),
        ToolChoice::Any => json!({"type": "any"}),
        ToolChoice::Tool(name) => json!({"type": "tool", "name": name}),
        ToolChoice::None => json!({"type": "none"}),
    }
}

/// Strip the `allOf`/`oneOf`/`anyOf` keywords Anthropic rejects at the
/// top level of an `input_schema`. Tools generated by external schema
/// generators sometimes include them; the kernel never validates schemas
/// itself so we sanitize here.
fn sanitize_schema(schema: &Value) -> Value {
    let Some(obj) = schema.as_object() else {
        return schema.clone();
    };
    if !ANTHROPIC_BLOCKED_SCHEMA_KEYWORDS
        .iter()
        .any(|k| obj.contains_key(*k))
    {
        return schema.clone();
    }
    let mut cleaned = obj.clone();
    for k in ANTHROPIC_BLOCKED_SCHEMA_KEYWORDS {
        cleaned.remove(k);
    }
    Value::Object(cleaned)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod web_search_tests;
