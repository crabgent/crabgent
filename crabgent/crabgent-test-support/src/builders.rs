//! Message, content-block, and response builders.
//!
//! These free functions mirror the hand-rolled `done`/`user_msg`/`assistant`
//! helpers that were duplicated across crate test modules, so call sites can
//! switch to the shared crate without changing their argument shapes.

use crabgent_core::{ContentBlock, LlmResponse, Message, ModelId, StopReason, ToolCall, Usage};
use serde_json::Value;

/// A plain-text user message with no authoring timestamp.
///
/// Replaces the per-file `fn user_msg(text: &str) -> Message` helpers.
#[must_use]
pub fn user_msg(text: impl Into<String>) -> Message {
    Message::user(vec![text_block(text)])
}

/// An assistant turn carrying only text and no tool calls.
///
/// Replaces the per-file `fn assistant(text: &str) -> Message` helpers.
#[must_use]
pub fn assistant(text: impl Into<String>) -> Message {
    Message::Assistant {
        text: text.into(),
        tool_calls: Vec::new(),
    }
}

/// An assistant turn carrying tool calls (and optional text).
#[must_use]
pub fn assistant_with_tools(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Message {
    Message::Assistant {
        text: text.into(),
        tool_calls,
    }
}

/// A `ContentBlock::Text` block.
#[must_use]
pub fn text_block(text: impl Into<String>) -> ContentBlock {
    ContentBlock::Text { text: text.into() }
}

/// A `ToolCall` with the given id, name, and JSON args.
///
/// `thought_signature` is left `None`; tests that need a provider
/// reasoning-correlation token set it on the returned value.
#[must_use]
pub fn tool_call(id: impl Into<String>, name: impl Into<String>, args: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        args,
        thought_signature: None,
    }
}

/// An end-of-turn `LlmResponse` carrying `text` and no tool calls.
///
/// The `model` field is set to `"m"`, matching the dominant convention in the
/// duplicated `fn done(text: &str) -> LlmResponse` helpers. Use
/// [`done_for_model`] when the response model id has to line up with a
/// specific registered model.
#[must_use]
pub fn done(text: impl Into<String>) -> LlmResponse {
    done_for_model(text, "m")
}

/// An end-of-turn `LlmResponse` with an explicit response model id.
#[must_use]
pub fn done_for_model(text: impl Into<String>, model: impl Into<ModelId>) -> LlmResponse {
    LlmResponse {
        text: text.into(),
        tool_calls: Vec::new(),
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        model: model.into(),
    }
}

/// A tool-use `LlmResponse`: empty text, the given tool calls, and
/// `StopReason::ToolUse`.
#[must_use]
pub fn tool_use(tool_calls: Vec<ToolCall>) -> LlmResponse {
    LlmResponse {
        text: String::new(),
        tool_calls,
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        model: "m".into(),
    }
}
