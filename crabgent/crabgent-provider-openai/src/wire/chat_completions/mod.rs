//! `/v1/chat/completions` wire format.

pub mod sse;

use crabgent_core::{
    LlmRequest, LlmResponse, ProjectedContent, ProjectedToolCall, ProjectedTurn, ProviderEvent,
    RunCtx, StopReason, ToolCall, ToolChoice, ToolDef, Usage, project_conversation,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::types::OpenAiError;
use crate::wire::WireFormat;
use crate::wire::chat_completions::sse::{ChatCompletionsStreamState, parse_sse_event};

const ENDPOINT_PATH: &str = "/v1/chat/completions";

/// Standard `OpenAI` Chat Completions wire format.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChatCompletionsWire;

impl WireFormat for ChatCompletionsWire {
    type StreamState = ChatCompletionsStreamState;

    fn endpoint_path(&self) -> &str {
        ENDPOINT_PATH
    }

    fn build_body(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        stream: bool,
    ) -> Result<Value, OpenAiError> {
        if req.web_search.enabled {
            return Err(OpenAiError::WebSearchUnsupportedOnChatCompletions);
        }
        let mut body = Map::new();
        body.insert("model".to_owned(), json!(req.model));
        body.insert("messages".to_owned(), Value::Array(transform_messages(req)));
        body.insert("stream".to_owned(), Value::Bool(stream));
        if let Some(max_tokens) = req.max_tokens {
            // GPT-5.x and o-family reasoning models reject the legacy
            // `max_tokens` parameter; the chat-completions endpoint
            // requires `max_completion_tokens`. Older models (gpt-4o,
            // gpt-4-turbo) accept either, so emit the new name
            // unconditionally for forward compatibility.
            body.insert("max_completion_tokens".to_owned(), json!(max_tokens));
        }
        if let Some(temperature) = req.temperature
            && !is_reasoning_model(req.model.as_str())
        {
            // GPT-5.x and o-family pin temperature to the default (1)
            // and 400 on any explicit value (including 0.0 from the
            // compact hook). Skip emission entirely for those models;
            // older models keep the caller's value.
            body.insert("temperature".to_owned(), json!(temperature));
        }
        if let Some(effort) = req.reasoning_effort {
            // Chat Completions expects a top-level `reasoning_effort`
            // string for o-series and gpt-5.x; the Responses API uses a
            // nested object instead.
            body.insert("reasoning_effort".to_owned(), json!(effort.as_str()));
        }
        // Scope OpenAI's automatic prompt caching to a deterministic key
        // derived from instructions + advertised tools. Identical key
        // strings across calls bind both to the same prefix cache; the
        // server still requires the byte prefix itself to match so this
        // is an explicit cache hint, not a free hit.
        let instructions = system_text(req);
        let tool_names: Vec<&str> = req.tools.iter().map(|tool| tool.name.as_str()).collect();
        body.insert(
            "prompt_cache_key".to_owned(),
            Value::String(crate::wire::responses::prompt_cache_key(
                &instructions,
                &tool_names,
            )),
        );
        if !req.stop_sequences.is_empty() {
            body.insert("stop".to_owned(), json!(req.stop_sequences));
        }
        let tools = tools_to_chat_completions(&req.tools);
        if !tools.is_empty() {
            body.insert("tools".to_owned(), Value::Array(tools));
            // Map request tool_choice to the Chat Completions wire shape. When
            // unset, omit the field: the server default is "auto".
            if let Some(tc) = &req.tool_choice {
                body.insert(
                    "tool_choice".to_owned(),
                    match tc {
                        ToolChoice::Auto => Value::String("auto".to_owned()),
                        ToolChoice::Any => Value::String("required".to_owned()),
                        ToolChoice::None => Value::String("none".to_owned()),
                        ToolChoice::Tool(name) => {
                            json!({"type": "function", "function": {"name": name}})
                        }
                    },
                );
            }
        }
        Ok(Value::Object(body))
    }

    fn parse_response(&self, body: Value) -> Result<LlmResponse, OpenAiError> {
        parse_response_body(body)
    }

    fn parse_sse_event(&self, line: &str, state: &mut Self::StreamState) -> Option<ProviderEvent> {
        parse_sse_event(line, state)
    }
}

fn system_text(req: &LlmRequest) -> String {
    req.system_prompt.clone().unwrap_or_default()
}

fn transform_messages(req: &LlmRequest) -> Vec<Value> {
    let mut messages =
        Vec::with_capacity(req.messages.len() + usize::from(req.system_prompt.is_some()));
    if let Some(system) = &req.system_prompt {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.extend(req.messages.iter().cloned());
    project_conversation(&messages)
        .iter()
        .filter_map(transform_turn)
        .collect()
}

/// Models that reject explicit `temperature`/`max_tokens` parameters and
/// require `max_completion_tokens`. Detected by id prefix because the
/// crabgent catalog covers both reasoning (gpt-5.x, o-series) and legacy
/// (gpt-4.x) variants in the same provider.
fn is_reasoning_model(model: &str) -> bool {
    model.starts_with("gpt-5") || model.starts_with("o1") || model.starts_with("o3")
}

fn transform_turn(turn: &ProjectedTurn) -> Option<Value> {
    match turn {
        ProjectedTurn::System { content } => transform_system_message(content.as_deref()),
        ProjectedTurn::User { content, raw } => transform_user_message(content.as_ref(), raw),
        ProjectedTurn::Assistant { text, tool_calls } => {
            Some(transform_assistant_message(text, tool_calls))
        }
        ProjectedTurn::ToolResult {
            call_id, output, ..
        } => Some(transform_tool_result_message(call_id, output)),
        ProjectedTurn::ChannelOutbound { body } => Some(transform_channel_outbound_message(body)),
        ProjectedTurn::Unknown {
            role: Some(_), raw, ..
        } => Some(raw.clone()),
        // ProviderBlock: not supported on Chat Completions. The web_search
        // guard in build_body prevents reaching this path with enabled
        // web_search; any stale ProviderBlock in history is dropped.
        _ => None,
    }
}

fn transform_system_message(content: Option<&str>) -> Option<Value> {
    let content = content?;
    Some(json!({"role": "system", "content": content}))
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
        ProjectedContent::Image { mime, data, .. } => Some(transform_user_image_block(mime, data)),
        ProjectedContent::Other(raw) if is_audio_block(raw) => transform_user_audio_block(raw),
        ProjectedContent::Text { raw, .. } | ProjectedContent::Other(raw) => Some(raw.clone()),
        ProjectedContent::Raw(_) | ProjectedContent::Blocks(_) => None,
    }
}

fn transform_user_image_block(mime: &str, data: &str) -> Value {
    json!({
        "type": "image_url",
        "image_url": {
            "url": format!("data:{mime};base64,{data}"),
            "detail": "auto",
        },
    })
}

fn is_audio_block(raw: &Value) -> bool {
    raw.get("type").and_then(Value::as_str) == Some("audio")
}

/// Reshape a neutral audio block into the Chat Completions `input_audio`
/// content part. The endpoint accepts only `wav` and `mp3`; any other
/// format is dropped with a warning rather than sent as an invalid block.
fn transform_user_audio_block(raw: &Value) -> Option<Value> {
    let mime = raw.get("mime").and_then(Value::as_str)?;
    let data = raw.get("data").and_then(Value::as_str)?;
    if let Some(format) = audio_mime_to_format(mime) {
        Some(json!({
            "type": "input_audio",
            "input_audio": { "data": data, "format": format },
        }))
    } else {
        crabgent_log::warn!(
            mime,
            "openai chat completions dropped audio: only wav and mp3 are accepted"
        );
        None
    }
}

fn audio_mime_to_format(mime: &str) -> Option<&'static str> {
    match mime {
        "audio/wav" | "audio/x-wav" => Some("wav"),
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        _ => None,
    }
}

fn transform_assistant_message(text: &str, calls: &[ProjectedToolCall]) -> Value {
    let mut transformed = json!({"role": "assistant", "content": text});
    let tool_calls: Vec<Value> = calls
        .iter()
        .filter_map(transform_assistant_tool_call)
        .collect();
    if !tool_calls.is_empty() {
        set_object_field(&mut transformed, "tool_calls", Value::Array(tool_calls));
    }
    transformed
}

fn set_object_field(value: &mut Value, key: &str, field: Value) {
    if let Some(object) = value.as_object_mut() {
        object.insert(key.to_owned(), field);
    }
}

fn transform_assistant_tool_call(call: &ProjectedToolCall) -> Option<Value> {
    let name = call.name.as_deref()?;
    let arguments = serde_json::to_string(&call.args).unwrap_or_else(|_| "{}".to_owned());
    Some(json!({
        "id": call.id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments,
        },
    }))
}

fn transform_tool_result_message(call_id: &str, output: &Value) -> Value {
    let content = value_to_string(output);
    json!({"role": "tool", "tool_call_id": call_id, "content": content})
}

fn transform_channel_outbound_message(body: &str) -> Value {
    json!({"role": "assistant", "content": body})
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn tools_to_chat_completions(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                },
            })
        })
        .collect()
}

fn parse_response_body(body: Value) -> Result<LlmResponse, OpenAiError> {
    let raw: RawResponse = serde_json::from_value(body)
        .map_err(|error| OpenAiError::MalformedResponse(error.to_string()))?;
    let choice =
        raw.choices.into_iter().next().ok_or_else(|| {
            OpenAiError::MalformedResponse("choices must not be empty".to_owned())
        })?;
    let message = choice.message;
    Ok(LlmResponse {
        text: message.content.unwrap_or_default(),
        tool_calls: parse_tool_calls(message.tool_calls)?,
        stop_reason: map_stop_reason(choice.finish_reason.as_deref()),
        usage: raw.usage.as_ref().map_or_else(Usage::default, parse_usage),
        model: raw.model.into(),
    })
}

fn parse_tool_calls(calls: Vec<RawToolCall>) -> Result<Vec<ToolCall>, OpenAiError> {
    calls
        .into_iter()
        .map(|call| {
            Ok(ToolCall {
                id: call.id,
                name: call.function.name,
                args: parse_arguments(&call.function.arguments)?,
                thought_signature: None,
            })
        })
        .collect()
}

pub(crate) fn parse_arguments(arguments: &str) -> Result<Value, OpenAiError> {
    serde_json::from_str(arguments)
        .map_err(|error| OpenAiError::MalformedResponse(error.to_string()))
}

pub(crate) fn map_stop_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("length") => StopReason::MaxTokens,
        Some("tool_calls") => StopReason::ToolUse,
        Some("stop") | None => StopReason::EndTurn,
        _ => StopReason::Other,
    }
}

const fn parse_usage(usage: &RawUsage) -> Usage {
    Usage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        cache_creation_tokens: 0,
        cache_read_tokens: usage.prompt_tokens_details.cached_tokens,
    }
}

#[derive(Debug, Deserialize)]
struct RawResponse {
    model: String,
    choices: Vec<RawChoice>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
struct RawChoice {
    message: RawMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<RawToolCall>,
}

#[derive(Debug, Deserialize)]
struct RawToolCall {
    id: String,
    function: RawFunctionCall,
}

#[derive(Debug, Deserialize)]
struct RawFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: RawPromptTokensDetails,
}

#[derive(Debug, Default, Deserialize)]
struct RawPromptTokensDetails {
    #[serde(default)]
    cached_tokens: u32,
}
