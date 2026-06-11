use crabgent_core::{ProviderEvent, StopReason, ToolCall};
use serde_json::Value;

use super::{ParserLimits, ParserResult, SseError};

pub(super) fn finalize_tool_use(
    id: String,
    name: String,
    input_json: &str,
    poisoned: bool,
) -> ParserResult {
    if id.trim().is_empty() {
        return Err(SseError::fatal("tool_use block missing id"));
    }
    if name.trim().is_empty() {
        return Err(SseError::fatal("tool_use block missing name"));
    }
    if poisoned {
        return Err(SseError::fatal(format!(
            "tool '{name}': input was truncated during streaming"
        )));
    }
    let json_str = if input_json.is_empty() {
        "{}"
    } else {
        input_json
    };
    let args = serde_json::from_str(json_str)
        .map_err(|e| SseError::fatal(format!("tool '{name}': malformed input JSON: {e}")))?;
    Ok(ProviderEvent::ToolUse(ToolCall {
        id,
        name,
        args,
        thought_signature: None,
    }))
}

pub fn append_within_limits(
    buf: &mut String,
    chunk: &str,
    limits: &ParserLimits,
    total_bytes: &mut usize,
) -> bool {
    if buf.len() + chunk.len() > limits.block_content_bytes
        || *total_bytes + chunk.len() > limits.total_content_bytes
    {
        return false;
    }
    buf.push_str(chunk);
    *total_bytes += chunk.len();
    true
}

pub fn map_stop_reason(raw: &str) -> StopReason {
    match raw {
        "end_turn" | "" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        _ => StopReason::Other,
    }
}

pub(super) fn is_error_stop_reason(raw: &str) -> bool {
    raw == "error" || raw.starts_with("error:")
}

pub fn parse_index(parsed: &Value) -> Option<usize> {
    parsed
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

pub(super) fn block_field(block: &Value, key: &str) -> String {
    block
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
