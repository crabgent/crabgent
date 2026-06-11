//! Provider-neutral projection of loose conversation JSON.
//!
//! Providers still own their endpoint-specific wire shapes. This module only
//! classifies crabgent's neutral message JSON and removes orphaned tool-call
//! pairs once before provider-specific mapping.

use std::collections::HashSet;

use serde_json::{Map, Value};

/// A cleaned, provider-neutral conversation turn.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProjectedTurn {
    /// System prompt-like message from the raw conversation.
    System { content: Option<String> },
    /// User-authored message.
    User {
        content: Option<ProjectedContent>,
        raw: Value,
    },
    /// Assistant-authored message.
    Assistant {
        text: String,
        tool_calls: Vec<ProjectedToolCall>,
    },
    /// Tool result sent back to the model.
    ToolResult {
        call_id: String,
        output: Value,
        is_error: bool,
    },
    /// Channel delivery audit record.
    ChannelOutbound { body: String },
    /// Message with an unknown or missing role.
    Unknown { role: Option<String>, raw: Value },
    /// Raw provider block from a server-side tool (echo in multi-turn
    /// conversations so the provider can correlate its own tool results).
    ProviderBlock { provider: String, block: Value },
}

/// Projected user content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectedContent {
    /// Non-array content value.
    Raw(Value),
    /// Array content projected block-by-block.
    Blocks(Vec<Self>),
    /// Parsed text block.
    Text { text: String, raw: Value },
    /// Parsed image block with already encoded payload data.
    Image {
        mime: String,
        data: String,
        raw: Value,
    },
    /// Unknown or malformed block.
    Other(Value),
}

/// Provider-neutral assistant tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedToolCall {
    pub id: String,
    pub name: Option<String>,
    pub args: Value,
    pub thought_signature: Option<String>,
}

/// Classify and clean loose neutral message JSON for provider request builders.
///
/// Assistant `tool_calls[*].id` and `tool_result.call_id` are intersected once:
/// orphan tool calls are removed from assistant turns, orphan tool results are
/// dropped, assistant text is kept when every call is removed, and empty
/// assistant turns are dropped.
#[must_use]
pub fn project_conversation(messages: &[Value]) -> Vec<ProjectedTurn> {
    let turns: Vec<ProjectedTurn> = messages.iter().map(project_turn).collect();
    drop_orphan_tool_pairs(turns)
}

fn project_turn(message: &Value) -> ProjectedTurn {
    let Some(role) = message.get("role").and_then(Value::as_str) else {
        return ProjectedTurn::Unknown {
            role: None,
            raw: message.clone(),
        };
    };
    match role {
        "system" => ProjectedTurn::System {
            content: system_content(message),
        },
        "user" => ProjectedTurn::User {
            content: message.get("content").map(project_user_content),
            raw: message.clone(),
        },
        "assistant" => ProjectedTurn::Assistant {
            text: message
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            tool_calls: message
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(|calls| calls.iter().filter_map(project_tool_call).collect())
                .unwrap_or_default(),
        },
        "tool_result" => ProjectedTurn::ToolResult {
            call_id: message
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            output: message
                .get("output")
                .cloned()
                .unwrap_or_else(|| Value::String(String::new())),
            is_error: message
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "channel_outbound" => ProjectedTurn::ChannelOutbound {
            body: message
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        },
        "provider_block" => ProjectedTurn::ProviderBlock {
            provider: message
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            block: message.get("block").cloned().unwrap_or(Value::Null),
        },
        other => ProjectedTurn::Unknown {
            role: Some(other.to_owned()),
            raw: message.clone(),
        },
    }
}

fn system_content(message: &Value) -> Option<String> {
    message
        .get("content")
        .and_then(Value::as_str)
        .or_else(|| message.get("text").and_then(Value::as_str))
        .map(str::to_owned)
}

fn project_user_content(content: &Value) -> ProjectedContent {
    if let Some(blocks) = content.as_array() {
        return ProjectedContent::Blocks(blocks.iter().map(project_block).collect());
    }
    ProjectedContent::Raw(content.clone())
}

fn project_block(block: &Value) -> ProjectedContent {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block.get("text").and_then(Value::as_str).map_or_else(
            || ProjectedContent::Other(block.clone()),
            |text| ProjectedContent::Text {
                text: text.to_owned(),
                raw: block.clone(),
            },
        ),
        Some("image") => {
            let mime = block.get("mime").and_then(Value::as_str);
            let data = block.get("data").and_then(Value::as_str);
            match (mime, data) {
                (Some(mime), Some(data)) => ProjectedContent::Image {
                    mime: mime.to_owned(),
                    data: data.to_owned(),
                    raw: block.clone(),
                },
                _ => ProjectedContent::Other(block.clone()),
            }
        }
        // A transcript reaches the chat model as plain text. The
        // `source_audio` handle and `voice` signals are stripped here so
        // no provider ever receives an unknown `transcript` block; the
        // audio side channel re-fetches bytes via the handle separately. A
        // degenerate transcript without a `text` key still projects to an
        // (empty) text block rather than leaking the raw transcript blob.
        Some("transcript") => {
            let text = block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            ProjectedContent::Text {
                text: text.to_owned(),
                raw: serde_json::json!({ "type": "text", "text": text }),
            }
        }
        _ => ProjectedContent::Other(block.clone()),
    }
}

fn project_tool_call(call: &Value) -> Option<ProjectedToolCall> {
    let id = call.get("id").and_then(Value::as_str)?.to_owned();
    Some(ProjectedToolCall {
        id,
        name: call.get("name").and_then(Value::as_str).map(str::to_owned),
        args: call
            .get("args")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new())),
        thought_signature: call
            .get("thought_signature")
            .or_else(|| call.get("thoughtSignature"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn drop_orphan_tool_pairs(turns: Vec<ProjectedTurn>) -> Vec<ProjectedTurn> {
    let mut tool_call_ids = HashSet::new();
    let mut tool_result_ids = HashSet::new();
    for turn in &turns {
        match turn {
            ProjectedTurn::Assistant { tool_calls, .. } => {
                tool_call_ids.extend(tool_calls.iter().map(|call| call.id.clone()));
            }
            ProjectedTurn::ToolResult { call_id, .. } => {
                tool_result_ids.insert(call_id.clone());
            }
            _ => {}
        }
    }
    let matched: HashSet<String> = tool_call_ids
        .intersection(&tool_result_ids)
        .cloned()
        .collect();

    turns
        .into_iter()
        .filter_map(|turn| match turn {
            ProjectedTurn::Assistant { text, tool_calls } => {
                let tool_calls: Vec<ProjectedToolCall> = tool_calls
                    .into_iter()
                    .filter(|call| matched.contains(&call.id))
                    .collect();
                if text.is_empty() && tool_calls.is_empty() {
                    None
                } else {
                    Some(ProjectedTurn::Assistant { text, tool_calls })
                }
            }
            ProjectedTurn::ToolResult {
                call_id,
                output,
                is_error,
            } => matched
                .contains(&call_id)
                .then_some(ProjectedTurn::ToolResult {
                    call_id,
                    output,
                    is_error,
                }),
            other => Some(other),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ProjectedContent, ProjectedTurn, project_conversation};

    #[test]
    fn projects_system_user_and_channel_shapes() {
        let turns = project_conversation(&[
            json!({"role": "system", "text": "policy"}),
            json!({"role": "user", "content": [
                {"type": "text", "text": "hi"},
                {"type": "image", "mime": "image/png", "data": "abc"}
            ]}),
            json!({"role": "channel_outbound", "body": "sent"}),
        ]);

        assert!(matches!(
            &turns[0],
            ProjectedTurn::System {
                content: Some(content)
            } if content == "policy"
        ));
        let ProjectedTurn::User {
            content: Some(ProjectedContent::Blocks(blocks)),
            ..
        } = &turns[1]
        else {
            panic!("expected projected user blocks");
        };
        assert!(matches!(&blocks[0], ProjectedContent::Text { text, .. } if text == "hi"));
        assert!(matches!(
            &blocks[1],
            ProjectedContent::Image { mime, data, .. }
                if mime == "image/png" && data == "abc"
        ));
        assert!(matches!(&turns[2], ProjectedTurn::ChannelOutbound { body } if body == "sent"));
    }

    #[test]
    fn transcript_block_projects_to_text_stripping_voice_and_source() {
        let turns = project_conversation(&[json!({"role": "user", "content": [
            {
                "type": "transcript",
                "text": "ja super",
                "source_audio": "ref-1",
                "voice": {"hesitation_count": 0}
            }
        ]})]);
        let ProjectedTurn::User {
            content: Some(ProjectedContent::Blocks(blocks)),
            ..
        } = &turns[0]
        else {
            panic!("expected projected user blocks");
        };
        let ProjectedContent::Text { text, raw } = &blocks[0] else {
            panic!("expected transcript projected as text, got {:?}", blocks[0]);
        };
        assert_eq!(text, "ja super");
        // The provider sees a clean text block: no source_audio / voice leak.
        assert_eq!(raw, &json!({"type": "text", "text": "ja super"}));
        assert!(raw.get("source_audio").is_none());
        assert!(raw.get("voice").is_none());
    }

    #[test]
    fn transcript_without_text_projects_to_empty_text_not_raw_blob() {
        let turns = project_conversation(&[json!({"role": "user", "content": [
            {"type": "transcript", "source_audio": "ref-1", "voice": {"hesitation_count": 0}}
        ]})]);
        let ProjectedTurn::User {
            content: Some(ProjectedContent::Blocks(blocks)),
            ..
        } = &turns[0]
        else {
            panic!("expected projected user blocks");
        };
        let ProjectedContent::Text { text, raw } = &blocks[0] else {
            panic!(
                "degenerate transcript must project as text, got {:?}",
                blocks[0]
            );
        };
        assert_eq!(text, "");
        assert_eq!(raw, &json!({"type": "text", "text": ""}));
        assert!(raw.get("source_audio").is_none());
    }

    #[test]
    fn projects_unknown_neutral_message_shapes() {
        let turns = project_conversation(&[
            json!({"role": "custom", "content": "kept"}),
            json!({"content": "missing role"}),
        ]);

        assert!(matches!(
            &turns[0],
            ProjectedTurn::Unknown {
                role: Some(role),
                ..
            } if role == "custom"
        ));
        assert!(matches!(
            &turns[1],
            ProjectedTurn::Unknown { role: None, .. }
        ));
    }

    #[test]
    fn drops_orphan_tool_pairs_and_empty_assistant_turns() {
        let turns = project_conversation(&[
            json!({
                "role": "assistant",
                "text": "checking",
                "tool_calls": [
                    {"id": "keep", "name": "search", "args": {"q": "kept"}},
                    {"id": "drop", "name": "search", "args": {"q": "drop"}}
                ]
            }),
            json!({"role": "tool_result", "call_id": "keep", "output": "ok"}),
            json!({
                "role": "assistant",
                "text": "",
                "tool_calls": [{"id": "orphan", "name": "search", "args": {}}]
            }),
            json!({"role": "tool_result", "call_id": "missing", "output": "stray"}),
        ]);

        assert_eq!(turns.len(), 2);
        let ProjectedTurn::Assistant { text, tool_calls } = &turns[0] else {
            panic!("expected assistant");
        };
        assert_eq!(text, "checking");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "keep");
        assert!(matches!(
            &turns[1],
            ProjectedTurn::ToolResult { call_id, .. } if call_id == "keep"
        ));
    }

    #[test]
    fn keeps_assistant_text_when_all_tool_calls_are_orphaned() {
        let turns = project_conversation(&[json!({
            "role": "assistant",
            "text": "text survives",
            "tool_calls": [{"id": "orphan", "name": "search", "args": {}}]
        })]);

        assert_eq!(turns.len(), 1);
        let ProjectedTurn::Assistant { text, tool_calls } = &turns[0] else {
            panic!("expected assistant");
        };
        assert_eq!(text, "text survives");
        assert!(tool_calls.is_empty());
    }
}
