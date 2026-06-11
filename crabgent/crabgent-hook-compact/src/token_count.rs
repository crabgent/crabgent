//! Token-cost estimation for compaction thresholds.
//!
//! Walks a message slice and accumulates a token estimate by routing every
//! text leaf through [`crabgent_core::tokens::estimate_tokens`] (the
//! workspace's single source of truth for token counts). Image blocks fold
//! into the per-image constant from `crabgent-core`.

use crabgent_core::tokens::{IMAGE_TOKENS, estimate_tokens as estimate_text_tokens};
use crabgent_core::{ContentBlock, Message, ToolCall};
use serde_json::Value;

/// Estimate provider-facing context tokens for a message slice.
#[must_use]
pub fn estimate_tokens(messages: &[Message]) -> usize {
    messages.iter().map(message_tokens).sum()
}

fn message_tokens(message: &Message) -> usize {
    match message {
        Message::System { content } => estimate_text_tokens(content),
        Message::User { content, .. } => content.iter().map(content_block_tokens).sum(),
        Message::Assistant { text, tool_calls } => {
            estimate_text_tokens(text) + tool_calls.iter().map(tool_call_tokens).sum::<usize>()
        }
        Message::ChannelOutbound { body, .. } => estimate_text_tokens(body),
        Message::ToolResult { output, .. } => value_tokens(output),
        _ => 0,
    }
}

fn content_block_tokens(block: &ContentBlock) -> usize {
    match block {
        // A transcript reaches the model as its recognized text, so it
        // counts toward the context budget like any other text leaf.
        ContentBlock::Text { text } | ContentBlock::Transcript { text, .. } => {
            estimate_text_tokens(text)
        }
        ContentBlock::Image(_) => IMAGE_TOKENS,
        _ => 0,
    }
}

fn tool_call_tokens(call: &ToolCall) -> usize {
    estimate_text_tokens(&call.id)
        + estimate_text_tokens(&call.name)
        + estimate_text_tokens(&render_json(&call.args))
}

fn value_tokens(value: &Value) -> usize {
    if is_image_value(value) {
        return IMAGE_TOKENS;
    }

    match value {
        Value::Null => 0,
        Value::Bool(flag) => estimate_text_tokens(&flag.to_string()),
        Value::Number(number) => estimate_text_tokens(&number.to_string()),
        Value::String(text) => estimate_text_tokens(text),
        Value::Array(values) => values.iter().map(value_tokens).sum(),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| estimate_text_tokens(key) + value_tokens(value))
            .sum(),
    }
}

fn is_image_value(value: &Value) -> bool {
    let Value::Object(map) = value else {
        return false;
    };
    let has_data = map.get("data").is_some();
    let has_image_type = map
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "image");
    let has_image_mime = map
        .get("mime")
        .and_then(Value::as_str)
        .is_some_and(|mime| mime.starts_with("image/"));
    has_data && (has_image_type || has_image_mime)
}

fn render_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[unserializable-json]".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::AudioRef;

    #[test]
    fn transcript_block_counts_its_text() {
        let block = ContentBlock::Transcript {
            text: "ja super".into(),
            source_audio: AudioRef::new("ref-1"),
            voice: None,
        };
        assert_eq!(
            content_block_tokens(&block),
            estimate_text_tokens("ja super")
        );
        assert!(content_block_tokens(&block) > 0);
    }

    #[test]
    fn user_message_with_transcript_counts_tokens() {
        let msg = Message::User {
            content: vec![ContentBlock::Transcript {
                text: "hello world".into(),
                source_audio: AudioRef::new("ref-2"),
                voice: None,
            }],
            timestamp: None,
        };
        assert_eq!(estimate_tokens(&[msg]), estimate_text_tokens("hello world"));
    }
}
