//! Wire-shaped chunk types for Slack `chat.startStream` and
//! `chat.appendStream` payloads, plus the stream-handle identity returned
//! by `chat.startStream`.
//!
//! Slack identifies a streaming message by the `(channel, ts)` tuple
//! returned from `chat.startStream`. There is no `stream_id`.

use serde::{Deserialize, Serialize};

/// Plain markdown chunk; rendered as a single message segment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MarkdownTextChunk {
    /// Markdown body inserted into the stream.
    pub text: String,
}

/// Task source pointer surfaced with a task update.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskSource {
    /// Source kind discriminator (mapped to the wire field `type`).
    #[serde(rename = "type")]
    pub kind: String,
    /// Human-readable source label.
    pub text: String,
    /// Source URL.
    pub url: String,
}

/// Lifecycle status reported on `task_update` chunks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task has not started.
    Pending,
    /// Task is actively running.
    InProgress,
    /// Task finished successfully.
    Complete,
    /// Task failed.
    Error,
}

/// Task progress chunk with optional details, output, and sources.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskUpdateChunk {
    /// Stable task identifier within the stream.
    pub id: String,
    /// Short task title.
    pub title: String,
    /// Current lifecycle status.
    pub status: TaskStatus,
    /// Optional progress detail body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    /// Optional final-output body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Optional source citations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<TaskSource>>,
}

/// High-level plan-update chunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUpdateChunk {
    /// Plan title rendered to the user.
    pub title: String,
}

/// Raw Block Kit blocks chunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlocksChunk {
    /// JSON-encoded `blocks` array forwarded verbatim to Slack.
    pub blocks: serde_json::Value,
}

/// Tagged stream chunk surfaced by `chat.startStream` /
/// `chat.appendStream` / `chat.stopStream`.
///
/// Variants are tagged by the wire field `type` and `rename_all =
/// "snake_case"` so the wire payload matches Slack's documented chunk
/// dispatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamChunk {
    /// Inline markdown body.
    MarkdownText(MarkdownTextChunk),
    /// Task progress envelope.
    TaskUpdate(TaskUpdateChunk),
    /// Plan-level update envelope.
    PlanUpdate(PlanUpdateChunk),
    /// Raw Block Kit blocks envelope.
    Blocks(BlocksChunk),
}

/// Identity of an active stream message.
///
/// `chat.startStream` returns the tuple; `chat.appendStream` and
/// `chat.stopStream` accept it as their addressing key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamHandle {
    /// Slack channel id.
    pub channel: String,
    /// Slack message timestamp from `chat.startStream`.
    pub ts: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn markdown_text_chunk_round_trips() {
        let chunk = StreamChunk::MarkdownText(MarkdownTextChunk {
            text: "hello".to_owned(),
        });
        let value = serde_json::to_value(&chunk).expect("serialize markdown");
        assert_eq!(value, json!({"type": "markdown_text", "text": "hello"}));
        let back: StreamChunk = serde_json::from_value(value).expect("deserialize markdown");
        assert_eq!(back, chunk);
    }

    #[test]
    fn task_update_chunk_round_trips_full() {
        let chunk = StreamChunk::TaskUpdate(TaskUpdateChunk {
            id: "t1".to_owned(),
            title: "search".to_owned(),
            status: TaskStatus::InProgress,
            details: Some("running".to_owned()),
            output: None,
            sources: Some(vec![TaskSource {
                kind: "url".to_owned(),
                text: "Wikipedia".to_owned(),
                url: "https://en.wikipedia.org/".to_owned(),
            }]),
        });
        let value = serde_json::to_value(&chunk).expect("serialize task update");
        assert_eq!(value["type"], "task_update");
        assert_eq!(value["status"], "in_progress");
        assert_eq!(value["sources"][0]["type"], "url");
        let back: StreamChunk = serde_json::from_value(value).expect("deserialize task update");
        assert_eq!(back, chunk);
    }

    #[test]
    fn task_update_chunk_skips_none_optionals() {
        let chunk = StreamChunk::TaskUpdate(TaskUpdateChunk {
            id: "t1".to_owned(),
            title: "search".to_owned(),
            status: TaskStatus::Pending,
            details: None,
            output: None,
            sources: None,
        });
        let value = serde_json::to_value(&chunk).expect("serialize task update");
        assert!(value.get("details").is_none(), "details must be omitted");
        assert!(value.get("output").is_none(), "output must be omitted");
        assert!(value.get("sources").is_none(), "sources must be omitted");
    }

    #[test]
    fn plan_update_chunk_round_trips() {
        let chunk = StreamChunk::PlanUpdate(PlanUpdateChunk {
            title: "plan".to_owned(),
        });
        let value = serde_json::to_value(&chunk).expect("serialize plan");
        assert_eq!(value, json!({"type": "plan_update", "title": "plan"}));
        let back: StreamChunk = serde_json::from_value(value).expect("deserialize plan");
        assert_eq!(back, chunk);
    }

    #[test]
    fn blocks_chunk_round_trips() {
        let blocks = json!([{"type": "section", "text": {"type": "mrkdwn", "text": "hi"}}]);
        let chunk = StreamChunk::Blocks(BlocksChunk {
            blocks: blocks.clone(),
        });
        let value = serde_json::to_value(&chunk).expect("serialize blocks");
        assert_eq!(value["type"], "blocks");
        assert_eq!(value["blocks"], blocks);
        let back: StreamChunk = serde_json::from_value(value).expect("deserialize blocks");
        assert_eq!(back, chunk);
    }

    #[test]
    fn task_status_serializes_snake_case() {
        let cases = [
            (TaskStatus::Pending, "pending"),
            (TaskStatus::InProgress, "in_progress"),
            (TaskStatus::Complete, "complete"),
            (TaskStatus::Error, "error"),
        ];
        for (status, wire) in cases {
            let value = serde_json::to_value(status).expect("serialize status");
            assert_eq!(value, serde_json::Value::String(wire.to_owned()));
        }
    }

    #[test]
    fn stream_handle_holds_tuple_identity() {
        let handle = StreamHandle {
            channel: "C1".to_owned(),
            ts: "1234.5678".to_owned(),
        };
        assert_eq!(handle.channel, "C1");
        assert_eq!(handle.ts, "1234.5678");
    }
}
