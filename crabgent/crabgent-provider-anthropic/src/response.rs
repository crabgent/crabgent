//! Parse the Anthropic Messages API non-streaming response body
//! into an `LlmResponse`.

use crabgent_core::{LlmResponse, ModelId, ProviderError, StopReason, ToolCall, Usage};
use serde::Deserialize;
use serde_json::Value;

pub fn parse(body: &Value) -> Result<LlmResponse, ProviderError> {
    let raw: RawResponse = serde_json::from_value(body.clone())
        .map_err(|e| ProviderError::MalformedResponse(e.to_string()))?;
    require_non_empty(&raw.id, "id")?;
    require_non_empty(&raw.model, "model")?;
    if raw.content.is_empty() {
        return Err(ProviderError::MalformedResponse(
            "content must contain at least one block".into(),
        ));
    }
    let model = ModelId::new(raw.model);
    let stop_reason = parse_stop_reason(&raw.stop_reason);
    let usage = parse_usage(&raw.usage);
    let (text, tool_calls) = parse_content(raw.content)?;
    let response = LlmResponse {
        text,
        tool_calls,
        stop_reason,
        usage,
        model,
    };
    Ok(response)
}

#[derive(Debug, Deserialize)]
struct RawResponse {
    id: String,
    model: String,
    content: Vec<RawContentBlock>,
    stop_reason: String,
    usage: RawUsage,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(rename = "input_tokens")]
    input: u32,
    #[serde(rename = "output_tokens")]
    output: u32,
    #[serde(default)]
    #[serde(rename = "cache_creation_input_tokens")]
    cache_creation_input: u32,
    #[serde(default)]
    #[serde(rename = "cache_read_input_tokens")]
    cache_read_input: u32,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum RawContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

fn require_non_empty(value: &str, field: &str) -> Result<(), ProviderError> {
    if value.is_empty() {
        return Err(ProviderError::MalformedResponse(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

fn parse_stop_reason(stop_reason: &str) -> StopReason {
    match stop_reason {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        _ => StopReason::Other,
    }
}

const fn parse_usage(usage: &RawUsage) -> Usage {
    Usage {
        input_tokens: usage.input,
        output_tokens: usage.output,
        cache_creation_tokens: usage.cache_creation_input,
        cache_read_tokens: usage.cache_read_input,
    }
}

fn parse_content(blocks: Vec<RawContentBlock>) -> Result<(String, Vec<ToolCall>), ProviderError> {
    let mut text = String::new();
    let mut calls = Vec::new();
    for block in blocks {
        match block {
            RawContentBlock::Text { text: chunk } => {
                text.push_str(&chunk);
            }
            RawContentBlock::ToolUse { id, name, input } => {
                require_non_empty(&id, "content.tool_use.id")?;
                require_non_empty(&name, "content.tool_use.name")?;
                calls.push(ToolCall {
                    id,
                    name,
                    args: input,
                    thought_signature: None,
                });
            }
        }
    }
    Ok((text, calls))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn well_formed() -> Value {
        json!({
            "id": "msg_1",
            "model": "claude-sonnet-4-6",
            "content": [{"type": "text", "text": "Hello world"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5},
        })
    }

    fn parse_ok(body: &Value) -> LlmResponse {
        parse(body).expect("parse response")
    }

    #[test]
    fn parses_text_only_response() {
        let body = well_formed();
        let r = parse_ok(&body);
        assert_eq!(r.text, "Hello world");
        assert!(r.tool_calls.is_empty());
        assert!(matches!(r.stop_reason, StopReason::EndTurn));
        assert_eq!(r.usage.input_tokens, 10);
        assert_eq!(r.usage.output_tokens, 5);
        assert_eq!(r.model.as_str(), "claude-sonnet-4-6");
    }

    #[test]
    fn parses_tool_use_response() {
        let body = json!({
            "id": "msg_1",
            "model": "claude",
            "content": [
                {"type": "text", "text": "calling tool"},
                {"type": "tool_use", "id": "toolu_1", "name": "search", "input": {"q": "x"}},
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 5},
        });
        let r = parse_ok(&body);
        assert_eq!(r.text, "calling tool");
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].id, "toolu_1");
        assert_eq!(r.tool_calls[0].name, "search");
        assert_eq!(r.tool_calls[0].args, json!({"q": "x"}));
        assert!(matches!(r.stop_reason, StopReason::ToolUse));
    }

    #[test]
    fn parser_rejects_missing_id() {
        let mut body = well_formed();
        body.as_object_mut().expect("test result").remove("id");
        assert!(matches!(
            parse(&body),
            Err(ProviderError::MalformedResponse(_))
        ));
    }

    #[test]
    fn parser_rejects_empty_content() {
        let mut body = well_formed();
        body["content"] = json!([]);
        assert!(matches!(
            parse(&body),
            Err(ProviderError::MalformedResponse(_))
        ));
    }

    #[test]
    fn parser_rejects_missing_usage_fields() {
        let mut body = well_formed();
        body["usage"] = json!({"output_tokens": 5});
        assert!(matches!(
            parse(&body),
            Err(ProviderError::MalformedResponse(_))
        ));
    }

    #[test]
    fn parser_rejects_missing_stop_reason() {
        let mut body = well_formed();
        body.as_object_mut()
            .expect("test result")
            .remove("stop_reason");
        assert!(matches!(
            parse(&body),
            Err(ProviderError::MalformedResponse(_))
        ));
    }

    #[test]
    fn unknown_stop_reason_maps_to_other() {
        let mut body = well_formed();
        body["stop_reason"] = json!("weird");
        let r = parse_ok(&body);
        assert!(matches!(r.stop_reason, StopReason::Other));
    }

    #[test]
    fn parser_accepts_well_formed() {
        assert_eq!(parse_ok(&well_formed()).text, "Hello world");
    }

    #[test]
    fn cache_token_fields_parsed() {
        let mut body = well_formed();
        body["usage"] = json!({
            "input_tokens": 1,
            "output_tokens": 2,
            "cache_creation_input_tokens": 7,
            "cache_read_input_tokens": 3,
        });
        let r = parse_ok(&body);
        assert_eq!(r.usage.cache_creation_tokens, 7);
        assert_eq!(r.usage.cache_read_tokens, 3);
    }

    #[test]
    fn parse_usage_maps_cache_tokens() {
        let mut body = well_formed();
        body["usage"] = json!({
            "input_tokens": 1,
            "output_tokens": 2,
            "cache_creation_input_tokens": 500,
            "cache_read_input_tokens": 200,
        });
        let r = parse_ok(&body);
        assert_eq!(r.usage.cache_creation_tokens, 500);
        assert_eq!(r.usage.cache_read_tokens, 200);
    }

    #[test]
    fn multi_text_block_concatenates() {
        let mut body = well_formed();
        body["content"] = json!([
            {"type": "text", "text": "Hello "},
            {"type": "text", "text": "world"},
        ]);
        let r = parse_ok(&body);
        assert_eq!(r.text, "Hello world");
    }

    #[test]
    fn unknown_block_type_is_rejected() {
        let mut body = well_formed();
        body["content"] = json!([
            {"type": "thinking", "thinking": "hmm"},
            {"type": "text", "text": "ok"},
        ]);
        assert!(matches!(
            parse(&body),
            Err(ProviderError::MalformedResponse(_))
        ));
    }

    #[test]
    fn tool_use_with_missing_input_is_rejected() {
        let mut body = well_formed();
        body["content"] = json!([{"type": "tool_use", "id": "i", "name": "n"}]);
        assert!(matches!(
            parse(&body),
            Err(ProviderError::MalformedResponse(_))
        ));
    }

    #[test]
    fn stop_reason_max_tokens_and_stop_sequence_recognised() {
        let mut body = well_formed();
        body["stop_reason"] = json!("max_tokens");
        assert!(matches!(parse_ok(&body).stop_reason, StopReason::MaxTokens));
        body["stop_reason"] = json!("stop_sequence");
        assert!(matches!(
            parse_ok(&body).stop_reason,
            StopReason::StopSequence
        ));
    }
}
