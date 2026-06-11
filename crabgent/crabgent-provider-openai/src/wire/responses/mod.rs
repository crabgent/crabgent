//! `/backend-api/codex/responses` wire format.

pub mod sse;

use crabgent_core::{
    LlmRequest, LlmResponse, ProjectedContent, ProjectedToolCall, ProjectedTurn, ProviderEvent,
    RunCtx, StopReason, ToolCall, ToolChoice, Usage, project_conversation,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::auth::CODEX_INSTALLATION_ID;
use crate::types::OpenAiError;
use crate::wire::WireFormat;
use crate::wire::responses::sse::{ResponsesStreamState, parse_sse_event};

const ENDPOINT_PATH: &str = "/backend-api/codex/responses";

/// Codex OAuth Responses wire format.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResponsesWire;

impl WireFormat for ResponsesWire {
    type StreamState = ResponsesStreamState;

    fn endpoint_path(&self) -> &str {
        ENDPOINT_PATH
    }

    fn build_body(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        stream: bool,
    ) -> Result<Value, OpenAiError> {
        // Field-insertion order mirrors codex_cli_rs upstream so the byte
        // prefix between two identical calls stays stable. With
        // `serde_json` `preserve_order` enabled at the workspace level,
        // `serde_json::Map` is backed by `IndexMap`, which guarantees
        // insertion order in the serialised body.
        let mut body = Map::new();
        // The `model` field is still emitted even though `ResponsesWire`
        // currently targets only the Codex OAuth backend at
        // `chatgpt.com/backend-api/codex/responses`, which derives the
        // effective model from the access token and ignores the
        // request body `model`. Emission is kept for two reasons:
        // (a) prompt-cache prefix byte-stability across runs so the
        // byte-identical body produced by `codex_cli_rs` upstream
        // matches ours and the Codex cache hits; (b) forward
        // compatibility, in case Codex ever validates the field or
        // `ResponsesWire` is later attached to the public Responses
        // endpoint via a different `AuthStrategy` config. The bound
        // value already mirrors `req.model`, so no silent backend
        // mismatch can leak.
        body.insert("model".to_owned(), Value::String(req.model.to_string()));
        body.insert("store".to_owned(), Value::Bool(false));
        // Codex backend rejects `stream=false` ("Stream must be set to
        // true"). Force-pin the field regardless of the caller's request
        // so the auto-cache prefix matches across complete + stream
        // accumulation paths.
        let _ = stream;
        body.insert("stream".to_owned(), Value::Bool(true));
        if let Some(effort) = req.reasoning_effort {
            body.insert(
                "reasoning".to_owned(),
                json!({"effort": effort.as_str(), "summary": "auto"}),
            );
        }
        let instructions = req.system_prompt.as_deref().unwrap_or("");
        let tool_names: Vec<&str> = req.tools.iter().map(|tool| tool.name.as_str()).collect();
        body.insert(
            "prompt_cache_key".to_owned(),
            Value::String(prompt_cache_key(instructions, &tool_names)),
        );
        if !instructions.is_empty() {
            body.insert(
                "instructions".to_owned(),
                Value::String(instructions.to_owned()),
            );
        }
        body.insert(
            "client_metadata".to_owned(),
            json!({"x-codex-installation-id": CODEX_INSTALLATION_ID}),
        );
        let tools = tools_to_responses(req);
        if !tools.is_empty() {
            body.insert("tools".to_owned(), Value::Array(tools));
            // Map request tool_choice to the Responses wire shape. When unset,
            // omit the field: the server default is "auto".
            if let Some(tc) = &req.tool_choice {
                body.insert(
                    "tool_choice".to_owned(),
                    match tc {
                        ToolChoice::Auto => Value::String("auto".to_owned()),
                        ToolChoice::Any => Value::String("required".to_owned()),
                        ToolChoice::None => Value::String("none".to_owned()),
                        ToolChoice::Tool(name) => json!({"type": "function", "name": name}),
                    },
                );
            }
        }
        body.insert(
            "input".to_owned(),
            Value::Array(transform_input(&req.messages)),
        );
        // Codex backend rejects `max_output_tokens` and pins `temperature`
        // to 1.0 server-side. Skip both fields; the model's defaults
        // govern truncation and sampling.
        let _ = req.max_tokens;
        let _ = req.temperature;
        Ok(Value::Object(body))
    }

    fn parse_response(&self, body: Value) -> Result<LlmResponse, OpenAiError> {
        parse_response_body(body)
    }

    fn parse_sse_event(&self, line: &str, state: &mut Self::StreamState) -> Option<ProviderEvent> {
        parse_sse_event(line, state)
    }
}

/// Build a deterministic 32-hex-char cache key from the instructions blob and
/// the sorted tool-name list. The Codex Responses backend uses
/// `prompt_cache_key` to bind two calls to the same prefix cache regardless of
/// per-message byte drift, as long as the system prompt + tool surface stay
/// stable. The hash uses NUL bytes as separators to avoid trivial collisions
/// when an instructions string ends in the prefix of a tool name.
#[must_use]
pub fn prompt_cache_key(instructions: &str, tool_names: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(instructions.as_bytes());
    let mut sorted: Vec<&str> = tool_names.to_vec();
    sorted.sort_unstable();
    for name in sorted {
        hasher.update([0u8]);
        hasher.update(name.as_bytes());
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").expect("writing to String never fails");
    }
    hex
}

fn transform_input(messages: &[Value]) -> Vec<Value> {
    project_conversation(messages)
        .iter()
        .flat_map(transform_turn)
        .collect()
}

fn transform_turn(turn: &ProjectedTurn) -> Vec<Value> {
    match turn {
        ProjectedTurn::User { content, raw } => transform_user_message(content.as_ref(), raw)
            .into_iter()
            .collect(),
        ProjectedTurn::Assistant { text, tool_calls } => {
            transform_assistant_message(text, tool_calls)
        }
        ProjectedTurn::ToolResult {
            call_id, output, ..
        } => vec![transform_tool_result_message(call_id, output)],
        ProjectedTurn::Unknown { raw, .. } => vec![raw.clone()],
        ProjectedTurn::ProviderBlock { provider, block } => {
            // Only echo blocks from OpenAI. Blocks from other providers
            // (Anthropic, Google) carry incompatible wire shapes and must
            // be skipped. Verbatim emission lets the Responses backend
            // correlate multi-turn web-search results.
            if provider == "openai" {
                vec![block.clone()]
            } else {
                Vec::new()
            }
        }
        // System, ChannelOutbound + future non-exhaustive variants: skip.
        _ => Vec::new(),
    }
}

fn transform_user_message(content: Option<&ProjectedContent>, raw: &Value) -> Option<Value> {
    match content {
        Some(ProjectedContent::Raw(content)) => {
            Some(json!({"role": "user", "content": content.clone()}))
        }
        Some(ProjectedContent::Blocks(blocks)) => {
            let transformed: Vec<Value> = blocks.iter().filter_map(transform_user_block).collect();
            if transformed.is_empty() {
                None
            } else {
                Some(json!({"role": "user", "content": transformed}))
            }
        }
        None | Some(_) => Some(raw.clone()),
    }
}

fn transform_user_block(block: &ProjectedContent) -> Option<Value> {
    match block {
        ProjectedContent::Text { text, .. } => Some(json!({"type": "input_text", "text": text})),
        ProjectedContent::Image { mime, data, .. } => Some(transform_user_image_block(mime, data)),
        ProjectedContent::Other(raw) if is_malformed_special_block(raw) => None,
        ProjectedContent::Other(raw) => Some(raw.clone()),
        ProjectedContent::Raw(_) | ProjectedContent::Blocks(_) => None,
    }
}

fn transform_user_image_block(mime: &str, data: &str) -> Value {
    json!({
        "type": "input_image",
        "image_url": format!("data:{mime};base64,{data}"),
    })
}

fn transform_assistant_message(text: &str, calls: &[ProjectedToolCall]) -> Vec<Value> {
    let mut out = Vec::new();
    if !text.is_empty() {
        out.push(json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": text }],
            "status": "completed",
        }));
    }
    for call in calls {
        out.push(json!({
            "type": "function_call",
            "call_id": call.id,
            "name": call.name.as_deref().unwrap_or(""),
            "arguments": serde_json::to_string(&call.args).unwrap_or_else(|_| "{}".to_owned()),
        }));
    }
    out
}

fn transform_tool_result_message(call_id: &str, output: &Value) -> Value {
    json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": value_to_string(output),
    })
}

fn is_malformed_special_block(raw: &Value) -> bool {
    matches!(
        raw.get("type").and_then(Value::as_str),
        Some("text" | "image")
    )
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Build the `tools` array for the Responses wire.
///
/// User-defined function tools are emitted first. When
/// `req.web_search.enabled` is true, a `{"type":"web_search",
/// "external_web_access":true}` entry is appended, with an optional
/// `"filters"` object when at least one domain list is non-empty. The
/// Responses API only accepts one of
/// `allowed_domains` or `blocked_domains`; both non-empty is rejected by
/// the kernel pre-flight before reaching the provider, so no re-check is
/// needed here.
fn tools_to_responses(req: &LlmRequest) -> Vec<Value> {
    // Codex Responses applies "strict mode" by default which promotes
    // every property to required. That breaks Anthropic-style schemas
    // where optional fields stay genuinely optional (channel_send's
    // `thread_parent` is the canonical victim). Opt out so the
    // optional/required boundary in `input_schema` carries through.
    let mut tools: Vec<Value> = req
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
                "strict": Value::Null,
            })
        })
        .collect();

    if req.web_search.enabled {
        tools.push(web_search_tool(
            &req.web_search.allowed_domains,
            &req.web_search.blocked_domains,
        ));
    }

    tools
}

fn web_search_tool(allowed_domains: &[String], blocked_domains: &[String]) -> Value {
    if !allowed_domains.is_empty() {
        json!({
            "type": "web_search",
            "external_web_access": true,
            "filters": {"allowed_domains": allowed_domains},
        })
    } else if !blocked_domains.is_empty() {
        json!({
            "type": "web_search",
            "external_web_access": true,
            "filters": {"blocked_domains": blocked_domains},
        })
    } else {
        json!({
            "type": "web_search",
            "external_web_access": true,
        })
    }
}

fn parse_response_body(body: Value) -> Result<LlmResponse, OpenAiError> {
    let raw: RawResponse = serde_json::from_value(body)
        .map_err(|error| OpenAiError::MalformedResponse(error.to_string()))?;
    let (text, tool_calls) = parse_output(raw.output)?;
    Ok(LlmResponse {
        text,
        tool_calls,
        stop_reason: map_stop_reason(raw.status.as_deref()),
        usage: raw.usage.as_ref().map_or_else(Usage::default, parse_usage),
        model: raw.model.unwrap_or_default().into(),
    })
}

fn parse_output(items: Vec<RawOutputItem>) -> Result<(String, Vec<ToolCall>), OpenAiError> {
    let mut text = String::new();
    let mut calls = Vec::new();
    for item in items {
        match item {
            RawOutputItem::Message { content } => append_message_content(content, &mut text),
            RawOutputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => calls.push(ToolCall {
                id: call_id,
                name,
                args: parse_arguments(&arguments)?,
                thought_signature: None,
            }),
            RawOutputItem::Other => {}
        }
    }
    Ok((text, calls))
}

fn append_message_content(content: Vec<RawContentItem>, text: &mut String) {
    for item in content {
        if let RawContentItem::OutputText { text: chunk } = item {
            text.push_str(&chunk);
        }
    }
}

pub(crate) fn parse_arguments(arguments: &str) -> Result<Value, OpenAiError> {
    serde_json::from_str(arguments)
        .map_err(|error| OpenAiError::MalformedResponse(error.to_string()))
}

pub(crate) fn map_stop_reason(status: Option<&str>) -> StopReason {
    match status {
        Some("completed") | None => StopReason::EndTurn,
        Some("incomplete") => StopReason::MaxTokens,
        Some("requires_action") => StopReason::ToolUse,
        _ => StopReason::Other,
    }
}

const fn parse_usage(usage: &RawUsage) -> Usage {
    Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_tokens: 0,
        cache_read_tokens: usage.input_tokens_details.cached_tokens,
    }
}

#[derive(Debug, Deserialize)]
struct RawResponse {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    output: Vec<RawOutputItem>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum RawOutputItem {
    #[serde(rename = "message")]
    Message {
        #[serde(default)]
        content: Vec<RawContentItem>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum RawContentItem {
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    input_tokens_details: RawInputTokensDetails,
}

#[derive(Debug, Default, Deserialize)]
struct RawInputTokensDetails {
    #[serde(default)]
    cached_tokens: u32,
}
