//! SSE state for `/v1/chat/completions`.

use std::collections::{BTreeMap, VecDeque};

use crabgent_core::{ProviderEvent, StopReason, ToolCall, Usage};
use serde_json::{Value, json};

use crate::wire::chat_completions::{map_stop_reason, parse_arguments};

/// Streaming parser state for Chat Completions.
#[derive(Debug, Clone, Default)]
pub struct ChatCompletionsStreamState {
    pending_data: String,
    tool_calls: BTreeMap<usize, ToolCallBuilder>,
    queued: VecDeque<ProviderEvent>,
    stop_reason: Option<StopReason>,
}

#[derive(Debug, Clone, Default)]
struct ToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

/// Parse one SSE `data:` line or fragment.
pub fn parse_sse_event(
    line: &str,
    state: &mut ChatCompletionsStreamState,
) -> Option<ProviderEvent> {
    if let Some(event) = state.queued.pop_front() {
        return Some(event);
    }

    let data = extract_data(line)?;
    if data == "[DONE]" {
        queue_terminal_events(state);
        return state.queued.pop_front();
    }

    state.pending_data.push_str(data);
    let Ok(parsed) = serde_json::from_str::<Value>(&state.pending_data) else {
        return None;
    };
    state.pending_data.clear();
    absorb_chunk(&parsed, state);
    state.queued.pop_front()
}

fn extract_data(line: &str) -> Option<&str> {
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .strip_prefix("data:")
        .map(|data| data.strip_prefix(' ').unwrap_or(data))
        .or(Some(trimmed))
}

fn absorb_chunk(parsed: &Value, state: &mut ChatCompletionsStreamState) {
    if let Some(usage) = parsed.get("usage") {
        state
            .queued
            .push_back(ProviderEvent::Usage(parse_usage(usage)));
    }

    let Some(choice) = parsed
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return;
    };

    if let Some(delta) = choice.get("delta") {
        absorb_delta(delta, state);
    }
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        let stop_reason = map_stop_reason(Some(reason));
        state.stop_reason = Some(stop_reason);
        if matches!(stop_reason, StopReason::ToolUse) {
            queue_tool_calls(state);
        }
        state.queued.push_back(ProviderEvent::Stop(stop_reason));
    }
}

fn absorb_delta(delta: &Value, state: &mut ChatCompletionsStreamState) {
    if let Some(content) = delta.get("content").and_then(Value::as_str)
        && !content.is_empty()
    {
        state
            .queued
            .push_back(ProviderEvent::TextDelta(content.to_owned()));
    }

    if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for call in tool_calls {
            absorb_tool_call_delta(call, state);
        }
    }
}

fn absorb_tool_call_delta(call: &Value, state: &mut ChatCompletionsStreamState) {
    let Some(index) = call
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
    else {
        return;
    };
    let builder = state.tool_calls.entry(index).or_default();
    if let Some(id) = call.get("id").and_then(Value::as_str) {
        id.clone_into(&mut builder.id);
    }
    let Some(function) = call.get("function") else {
        return;
    };
    if let Some(name) = function.get("name").and_then(Value::as_str) {
        name.clone_into(&mut builder.name);
    }
    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
        builder.arguments.push_str(arguments);
    }
}

fn queue_terminal_events(state: &mut ChatCompletionsStreamState) {
    queue_tool_calls(state);
    state.queued.push_back(ProviderEvent::Stop(
        state.stop_reason.unwrap_or(StopReason::EndTurn),
    ));
}

fn queue_tool_calls(state: &mut ChatCompletionsStreamState) {
    let builders = std::mem::take(&mut state.tool_calls);
    for builder in builders.into_values() {
        state.queued.push_back(ProviderEvent::ToolUse(ToolCall {
            id: builder.id,
            name: builder.name,
            args: parse_arguments(&builder.arguments).unwrap_or_else(|_| json!({})),
            thought_signature: None,
        }));
    }
}

fn parse_usage(usage: &Value) -> Usage {
    Usage {
        input_tokens: get_u32(usage, "prompt_tokens"),
        output_tokens: get_u32(usage, "completion_tokens"),
        cache_creation_tokens: 0,
        cache_read_tokens: usage
            .get("prompt_tokens_details")
            .map_or(0, |details| get_u32(details, "cached_tokens")),
    }
}

fn get_u32(value: &Value, key: &str) -> u32 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}
