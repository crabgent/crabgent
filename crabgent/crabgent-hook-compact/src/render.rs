//! Rendering helpers for summary-provider prompts.

use std::fmt::{self, Write as _};

use crabgent_core::{ContentBlock, Message, ToolCall};
use serde_json::Value;

const SUMMARY_START_MARKER: &str = "<crabgent-compact-summary>";
const SUMMARY_END_MARKER: &str = "</crabgent-compact-summary>";

/// Append formatted arguments to an in-memory transcript buffer.
///
/// All transcript writes go through here so the infallibility invariant is
/// stated once instead of repeated at every call site.
fn push(out: &mut String, args: fmt::Arguments<'_>) {
    // invariant: `fmt::Write for String` never returns `Err`; the only failure
    // path of `write_fmt` is a `Display`/`Debug` impl that itself errors, and
    // none of the formatted values here have a fallible formatter.
    out.write_fmt(args)
        .expect("writing to an in-memory String cannot fail");
}

pub fn render_transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for (idx, message) in messages.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        render_message(&mut out, idx + 1, message);
    }
    out
}

pub fn render_summary_message(summary: &str, compacted_count: usize) -> Message {
    Message::User {
        content: vec![ContentBlock::Text {
            text: format!(
                "{SUMMARY_START_MARKER}\nCompacted {compacted_count} earlier messages.\n\n{}\n{SUMMARY_END_MARKER}",
                summary.trim()
            ),
        }],
        timestamp: None,
    }
}

pub fn is_summary_message(message: &Message) -> bool {
    let Message::User { content, .. } = message else {
        return false;
    };
    matches!(
        content.first(),
        // Only hook-generated summaries start with the marker; mid-text markers
        // can come from ordinary user or tool content.
        Some(ContentBlock::Text { text }) if text.starts_with(SUMMARY_START_MARKER)
    )
}

fn render_message(out: &mut String, number: usize, message: &Message) {
    push(out, format_args!("Message {number}:\n"));
    match message {
        Message::System { content } => render_text(out, "system", content),
        Message::User { content, .. } => render_user(out, content),
        Message::Assistant { text, tool_calls } => render_assistant(out, text, tool_calls),
        Message::ToolResult {
            call_id,
            output,
            is_error,
        } => render_tool_result(out, call_id, output, *is_error),
        Message::ChannelOutbound { body, .. } => render_assistant(out, body, &[]),
        _ => render_text(out, "unknown", "[unknown future message variant]"),
    }
}

fn render_text(out: &mut String, role: &str, text: &str) {
    push(out, format_args!("[{role}]\n"));
    push(out, format_args!("{text}\n"));
}

fn render_user(out: &mut String, content: &[ContentBlock]) {
    push(out, format_args!("[user]\n"));
    for block in content {
        match block {
            // A transcript renders as its recognized text, like a text block.
            ContentBlock::Text { text } | ContentBlock::Transcript { text, .. } => {
                push(out, format_args!("{text}\n"));
            }
            ContentBlock::Image(payload) => {
                push(
                    out,
                    format_args!(
                        "[image mime={} bytes={}]\n",
                        payload.mime(),
                        payload.bytes().len()
                    ),
                );
            }
            _ => {
                push(out, format_args!("[unknown future content block]\n"));
            }
        }
    }
}

fn render_assistant(out: &mut String, text: &str, tool_calls: &[ToolCall]) {
    push(out, format_args!("[assistant]\n"));
    if !text.is_empty() {
        push(out, format_args!("{text}\n"));
    }
    for call in tool_calls {
        push(
            out,
            format_args!(
                "[tool_call id={} name={} args={}]\n",
                call.id,
                call.name,
                render_json(&call.args)
            ),
        );
    }
}

fn render_tool_result(out: &mut String, call_id: &str, output: &Value, is_error: bool) {
    push(
        out,
        format_args!("[tool_result call_id={call_id} is_error={is_error}]\n"),
    );
    push(out, format_args!("{}\n", render_json(output)));
}

fn render_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[unserializable-json]".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::Owner;
    use crabgent_core::ToolCall;
    use serde_json::json;

    #[test]
    fn transcript_renders_roles_and_tool_data() {
        let messages = vec![
            Message::User {
                content: vec![ContentBlock::Text { text: "hi".into() }],
                timestamp: None,
            },
            Message::Assistant {
                text: "checking".into(),
                tool_calls: vec![ToolCall {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    args: json!({"path": "Cargo.toml"}),
                    thought_signature: None,
                }],
            },
            Message::ToolResult {
                call_id: "call-1".into(),
                output: json!({"ok": true}),
                is_error: false,
            },
        ];

        let transcript = render_transcript(&messages);

        assert!(transcript.contains("[user]"));
        assert!(transcript.contains("checking"));
        assert!(transcript.contains("read_file"));
        assert!(transcript.contains("\"ok\":true"));
    }

    #[test]
    fn transcript_block_renders_its_text_not_placeholder() {
        let messages = vec![Message::User {
            content: vec![ContentBlock::Transcript {
                text: "ja super".into(),
                source_audio: crabgent_core::AudioRef::new("ref-1"),
                voice: None,
            }],
            timestamp: None,
        }];

        let transcript = render_transcript(&messages);

        assert!(transcript.contains("ja super"), "{transcript}");
        assert!(
            !transcript.contains("[unknown future content block]"),
            "{transcript}"
        );
    }

    #[test]
    fn channel_outbound_renders_as_assistant_with_body() {
        let messages = vec![Message::ChannelOutbound {
            conv: Owner::new("slack:T1/C1"),
            body: "hello world".into(),
            channel: "slack".into(),
            message_id: "1234.5678".into(),
            thread_root: None,
            broadcast: false,
        }];
        let transcript = render_transcript(&messages);
        assert!(transcript.contains("[assistant]"));
        assert!(transcript.contains("hello world"));
    }

    #[test]
    fn summary_message_contains_markers_and_count() {
        let message = render_summary_message("short summary", 3);

        match message {
            Message::User { content, .. } => {
                let ContentBlock::Text { text } = &content[0] else {
                    panic!("summary content should be text");
                };
                assert!(text.starts_with(SUMMARY_START_MARKER));
                assert!(text.contains("Compacted 3 earlier messages."));
                assert!(text.contains("short summary"));
                assert!(text.contains(SUMMARY_END_MARKER));
            }
            _ => panic!("summary message should be user-visible"),
        }
    }

    #[test]
    fn is_summary_message_detects_marker_in_first_text_block() {
        let summary = render_summary_message("short summary", 3);
        let plain = Message::User {
            content: vec![ContentBlock::Text {
                text: "ordinary text".into(),
            }],
            timestamp: None,
        };
        let marker_not_first = Message::User {
            content: vec![
                ContentBlock::Image(
                    crabgent_core::ImagePayload::new(vec![1_u8], "image/png")
                        .expect("valid image payload"),
                ),
                ContentBlock::Text {
                    text: SUMMARY_START_MARKER.into(),
                },
            ],
            timestamp: None,
        };

        assert!(is_summary_message(&summary));
        assert!(!is_summary_message(&plain));
        assert!(!is_summary_message(&marker_not_first));
    }

    #[test]
    fn is_summary_message_uses_starts_with() {
        let marker_mid_text = Message::User {
            content: vec![ContentBlock::Text {
                text: format!("ordinary text with {SUMMARY_START_MARKER} inside"),
            }],
            timestamp: None,
        };

        assert!(!is_summary_message(&marker_mid_text));
    }
}
