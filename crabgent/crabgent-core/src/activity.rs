//! Bounded summaries for kernel streaming events.
//!
//! These types are safe to hand to background-work observers because they keep
//! lifecycle shape and byte counts while excluding raw tool args, tool output,
//! reasoning text, hosted-tool raw blocks, and channel bodies.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hook::{AttemptErrorClass, Event};
use crate::message::Message;
use crate::text::truncate_with_ellipsis;
use crate::types::{Notification, NotificationLevel, ToolCall, ToolResult};

pub const ACTIVITY_TEXT_PREVIEW_BYTES: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ActivityEventSummary {
    OutputDelta(ActivityTextSummary),
    ReasoningDelta(ActivityTextSummary),
    ToolCallStarted(ToolCallActivitySummary),
    ToolCallCompleted(ToolCallResultActivitySummary),
    Notification(NotificationActivitySummary),
    ServerToolResult(ServerToolResultActivitySummary),
    AttemptFailed(AttemptFailedActivitySummary),
    Final(ActivityTextSummary),
}

impl ActivityEventSummary {
    #[must_use]
    pub fn from_event(event: &Event) -> Self {
        match event {
            Event::Token(text) => Self::OutputDelta(ActivityTextSummary::with_preview(text)),
            Event::Reasoning(text) => Self::ReasoningDelta(ActivityTextSummary::redacted(text)),
            Event::ToolCallStarted(call) => Self::ToolCallStarted(call.into()),
            Event::ToolCallCompleted { call, result } => Self::ToolCallCompleted(
                ToolCallResultActivitySummary::from_call_result(call, result),
            ),
            Event::Notification(note) => Self::Notification(note.into()),
            Event::ServerToolResult {
                provider,
                name,
                citations,
                raw,
            } => Self::ServerToolResult(ServerToolResultActivitySummary {
                provider: provider.clone(),
                name: name.clone(),
                citation_count: citations.len(),
                raw: JsonShapeSummary::from_value(raw),
            }),
            Event::AttemptFailed {
                attempt_idx,
                total_attempts,
                provider,
                model,
                error_class,
                message,
                will_fallback,
            } => Self::AttemptFailed(AttemptFailedActivitySummary {
                attempt_idx: *attempt_idx,
                total_attempts: *total_attempts,
                provider: provider.clone(),
                model: model.clone(),
                error_class: error_class.clone(),
                message: ActivityTextSummary::redacted(message),
                will_fallback: *will_fallback,
            }),
            Event::Final(text) => Self::Final(ActivityTextSummary::with_preview(text)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityTextSummary {
    pub bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    pub truncated: bool,
}

impl ActivityTextSummary {
    #[must_use]
    pub fn with_preview(text: &str) -> Self {
        let preview = truncate_with_ellipsis(text, ACTIVITY_TEXT_PREVIEW_BYTES, "...");
        Self {
            bytes: text.len(),
            preview: Some(preview.into_owned()),
            truncated: text.len() > ACTIVITY_TEXT_PREVIEW_BYTES,
        }
    }

    #[must_use]
    pub const fn redacted(text: &str) -> Self {
        Self {
            bytes: text.len(),
            preview: None,
            truncated: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum JsonValueKind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonShapeSummary {
    pub kind: JsonValueKind,
    pub bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub array_len: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_len: Option<usize>,
}

impl JsonShapeSummary {
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        Self {
            kind: json_kind(value),
            bytes: json_approx_bytes(value),
            array_len: value.as_array().map(Vec::len),
            object_len: value.as_object().map(serde_json::Map::len),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallActivitySummary {
    pub call_id: String,
    pub tool_name: String,
    pub args: JsonShapeSummary,
    pub has_thought_signature: bool,
}

impl From<&ToolCall> for ToolCallActivitySummary {
    fn from(call: &ToolCall) -> Self {
        Self {
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            args: JsonShapeSummary::from_value(&call.args),
            has_thought_signature: call.thought_signature.is_some(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallResultActivitySummary {
    pub call_id: String,
    pub tool_name: String,
    pub is_error: bool,
    pub output: JsonShapeSummary,
    pub run_message_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_message_kinds: Vec<String>,
    pub channel_outbound_count: usize,
}

impl ToolCallResultActivitySummary {
    #[must_use]
    pub fn from_call_result(call: &ToolCall, result: &ToolResult) -> Self {
        Self {
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            is_error: result.is_error,
            output: JsonShapeSummary::from_value(&result.output),
            run_message_count: result.run_messages.len(),
            run_message_kinds: run_message_kinds(&result.run_messages),
            channel_outbound_count: channel_outbound_count(&result.run_messages),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationActivitySummary {
    pub kind: String,
    pub level: NotificationLevel,
    pub message: ActivityTextSummary,
}

impl From<&Notification> for NotificationActivitySummary {
    fn from(note: &Notification) -> Self {
        Self {
            kind: note.kind.clone(),
            level: note.level,
            message: ActivityTextSummary::redacted(&note.message),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerToolResultActivitySummary {
    pub provider: String,
    pub name: String,
    pub citation_count: usize,
    pub raw: JsonShapeSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptFailedActivitySummary {
    pub attempt_idx: usize,
    pub total_attempts: usize,
    pub provider: String,
    pub model: String,
    pub error_class: AttemptErrorClass,
    pub message: ActivityTextSummary,
    pub will_fallback: bool,
}

const fn json_kind(value: &Value) -> JsonValueKind {
    match value {
        Value::Null => JsonValueKind::Null,
        Value::Bool(_) => JsonValueKind::Bool,
        Value::Number(_) => JsonValueKind::Number,
        Value::String(_) => JsonValueKind::String,
        Value::Array(_) => JsonValueKind::Array,
        Value::Object(_) => JsonValueKind::Object,
    }
}

fn json_approx_bytes(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(v) => {
            if *v {
                4
            } else {
                5
            }
        }
        Value::Number(n) => n.to_string().len(),
        Value::String(s) => s.len(),
        Value::Array(values) => {
            2 + values.len().saturating_sub(1) + values.iter().map(json_approx_bytes).sum::<usize>()
        }
        Value::Object(map) => {
            2 + map.len().saturating_sub(1)
                + map
                    .iter()
                    .map(|(key, value)| key.len() + 1 + json_approx_bytes(value))
                    .sum::<usize>()
        }
    }
}

fn run_message_kinds(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(message_kind)
        .map(str::to_owned)
        .collect()
}

const fn message_kind(message: &Message) -> &'static str {
    match message {
        Message::System { .. } => "system",
        Message::User { .. } => "user",
        Message::Assistant { .. } => "assistant",
        Message::ToolResult { .. } => "tool_result",
        Message::ChannelOutbound { .. } => "channel_outbound",
        Message::ProviderBlock { .. } => "provider_block",
    }
}

fn channel_outbound_count(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter(|message| matches!(message, Message::ChannelOutbound { .. }))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::{Notification, NotificationLevel, ToolCall, ToolResult};

    #[test]
    fn tool_call_summary_excludes_arg_values_and_thought_signature() {
        let event = Event::ToolCallStarted(ToolCall {
            id: "call-1".into(),
            name: "task".into(),
            args: json!({"op": "create", "prompt": "secret text"}),
            thought_signature: Some("opaque-secret".into()),
        });

        let summary = ActivityEventSummary::from_event(&event);
        let encoded = serde_json::to_string(&summary).expect("summary serializes");

        assert!(encoded.contains("call-1"));
        assert!(encoded.contains("task"));
        assert!(!encoded.contains("create"));
        assert!(!encoded.contains("prompt"));
        assert!(!encoded.contains("secret text"));
        assert!(!encoded.contains("opaque-secret"));
    }

    #[test]
    fn tool_result_summary_excludes_output_values_and_channel_body() {
        let call = ToolCall {
            id: "call-1".into(),
            name: "channel_send".into(),
            args: json!({}),
            thought_signature: None,
        };
        let result = ToolResult::success(json!({"body": "private output"}))
            .with_call_id("call-1")
            .with_run_message(Message::ChannelOutbound {
                conv: crate::Owner::new("u"),
                body: "private channel body".into(),
                channel: "slack".into(),
                message_id: "msg-1".into(),
                thread_root: None,
                broadcast: false,
            });
        let summary = ActivityEventSummary::from_event(&Event::ToolCallCompleted { call, result });
        let encoded = serde_json::to_string(&summary).expect("summary serializes");

        assert!(encoded.contains("channel_outbound"));
        assert!(!encoded.contains("private output"));
        assert!(!encoded.contains("private channel body"));
        assert!(!encoded.contains("msg-1"));
    }

    #[test]
    fn notification_summary_excludes_message_text() {
        let summary = ActivityEventSummary::from_event(&Event::Notification(Notification {
            kind: "status".into(),
            message: "private notification".into(),
            level: NotificationLevel::Info,
        }));
        let encoded = serde_json::to_string(&summary).expect("summary serializes");

        assert!(encoded.contains("status"));
        assert!(!encoded.contains("private notification"));
    }

    #[test]
    fn server_tool_result_summary_excludes_raw_and_citation_text() {
        let summary = ActivityEventSummary::from_event(&Event::ServerToolResult {
            provider: "google".into(),
            name: "web_search".into(),
            citations: vec![crate::Citation {
                url: "https://example.test/private".into(),
                title: Some("private title".into()),
                cited_text: Some("private cited text".into()),
                provider: "google".into(),
                raw: json!({"private": "citation raw"}),
            }],
            raw: json!({"private": "server raw"}),
        });
        let encoded = serde_json::to_string(&summary).expect("summary serializes");

        assert!(encoded.contains("web_search"));
        assert!(!encoded.contains("server raw"));
        assert!(!encoded.contains("private cited text"));
        assert!(!encoded.contains("example.test"));
    }

    #[test]
    fn reasoning_summary_carries_only_size() {
        let summary = ActivityEventSummary::from_event(&Event::Reasoning("private thought".into()));
        let encoded = serde_json::to_string(&summary).expect("summary serializes");

        assert!(encoded.contains("\"bytes\""));
        assert!(!encoded.contains("private thought"));
    }
}
