//! Safe-prefix helpers for [`super::SessionPersistHook`]. Errored and
//! cancelled runs persist only the accepted user/input prefix, never a partial
//! assistant/tool tail. These pure functions compute that prefix.

use crabgent_core::message::Message;

/// True when the last message is user/system input, i.e. the whole log is a
/// safe prefix with no dangling assistant/tool tail.
pub(super) fn is_error_safe_log(msgs: &[Message]) -> bool {
    msgs.last()
        .is_some_and(|msg| matches!(msg, Message::System { .. } | Message::User { .. }))
}

/// Longest prefix of `msgs` that ends in a user/system message, i.e. the
/// accepted input with any trailing assistant/tool-call tail dropped. Empty
/// when no user/system message exists.
pub(super) fn safe_prefix(msgs: &[Message]) -> &[Message] {
    let Some(idx) = msgs
        .iter()
        .rposition(|msg| matches!(msg, Message::System { .. } | Message::User { .. }))
    else {
        return &[];
    };
    // `rposition` returns an in-bounds index, so `idx + 1` is a valid prefix
    // length; `get` keeps this slice panic-free for the restriction lint.
    msgs.get(..=idx).unwrap_or(msgs)
}

/// Persisted log for a paused run. Unlike [`safe_prefix`], every
/// completed turn is KEPT (the tail is the resume state a paused run
/// continues from), including resolved tool results whose side effects
/// already happened. Dangling tool calls (the pause landed between tool
/// dispatches) get a synthetic error result instead of being trimmed,
/// mirroring the task-side repair: interrupted calls are surfaced to the
/// model, never silently dropped or re-run.
pub(super) fn repaired_paused_log(msgs: &[Message]) -> Vec<Message> {
    let resolved: std::collections::HashSet<&str> = msgs
        .iter()
        .filter_map(|msg| match msg {
            Message::ToolResult { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    let dangling: Vec<String> = msgs
        .iter()
        .filter_map(|msg| match msg {
            Message::Assistant { tool_calls, .. } => Some(tool_calls),
            _ => None,
        })
        .flatten()
        .filter(|call| !resolved.contains(call.id.as_str()))
        .map(|call| call.id.clone())
        .collect();
    let mut repaired = msgs.to_vec();
    for call_id in dangling {
        repaired.push(Message::ToolResult {
            call_id,
            output: serde_json::Value::String(
                "interrupted by pause/restart before a result was recorded; \
                 the call may have partially executed. Decide what (if anything) to redo."
                    .to_owned(),
            ),
            is_error: true,
        });
    }
    repaired
}

/// Pick whichever safe prefix carries more messages: the snapshot captured at
/// the last safe `on_message`, or the trimmed latest log. The latest log is a
/// superset of earlier snapshots, so its safe prefix is normally at least as
/// complete; the snapshot is the fallback for the degenerate case where the
/// latest log lost a safe prefix the snapshot still holds.
pub(super) fn longest_safe_prefix(
    snapshot: Option<Vec<Message>>,
    latest: &[Message],
) -> Vec<Message> {
    let trimmed = safe_prefix(latest);
    match snapshot {
        Some(snap) if snap.len() > trimmed.len() => snap,
        _ => trimmed.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::message::ContentBlock;

    fn user(text: &str) -> Message {
        Message::User {
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            timestamp: None,
        }
    }

    fn assistant(text: &str) -> Message {
        Message::Assistant {
            text: text.to_owned(),
            tool_calls: Vec::new(),
        }
    }

    #[test]
    fn safe_prefix_drops_trailing_assistant_tail() {
        let log = vec![user("a"), assistant("partial")];
        let prefix = safe_prefix(&log);
        assert_eq!(prefix.len(), 1);
        assert!(matches!(prefix[0], Message::User { .. }));
    }

    #[test]
    fn safe_prefix_empty_when_no_user_or_system() {
        let log = vec![assistant("only assistant")];
        assert!(safe_prefix(&log).is_empty());
    }

    #[test]
    fn longest_safe_prefix_prefers_trimmed_latest_over_stale_snapshot() {
        // Stale empty snapshot from on_session_start vs latest log carrying
        // accepted user input behind an unsafe tail: the trimmed latest wins.
        let latest = vec![user("input"), assistant("partial")];
        let merged = longest_safe_prefix(Some(Vec::new()), &latest);
        assert_eq!(merged.len(), 1);
        assert!(matches!(merged[0], Message::User { .. }));
    }

    #[test]
    fn longest_safe_prefix_keeps_richer_snapshot_when_latest_lost_prefix() {
        let snapshot = vec![user("a"), user("b")];
        let latest = vec![assistant("tail only")];
        let merged = longest_safe_prefix(Some(snapshot), &latest);
        assert_eq!(merged.len(), 2);
    }

    fn assistant_with_call(id: &str) -> Message {
        Message::Assistant {
            text: "working".to_owned(),
            tool_calls: vec![crabgent_core::types::ToolCall {
                id: id.to_owned(),
                name: "bash".to_owned(),
                args: serde_json::json!({}),
                thought_signature: None,
            }],
        }
    }

    fn tool_result(call_id: &str) -> Message {
        Message::ToolResult {
            call_id: call_id.to_owned(),
            output: serde_json::json!("ok"),
            is_error: false,
        }
    }

    #[test]
    fn repaired_paused_log_keeps_completed_turns_untouched() {
        let log = vec![
            user("a"),
            assistant_with_call("c1"),
            tool_result("c1"),
            assistant("done thinking"),
        ];
        let repaired = repaired_paused_log(&log);
        assert_eq!(repaired.len(), 4, "boundary-clean log is kept whole");
    }

    #[test]
    fn repaired_paused_log_appends_synthetic_result_for_dangling_call() {
        let log = vec![
            user("a"),
            assistant_with_call("c1"),
            tool_result("c1"),
            assistant_with_call("c2"),
        ];
        let repaired = repaired_paused_log(&log);
        assert_eq!(repaired.len(), 5, "resolved c1 survives, c2 gets repaired");
        match repaired.last() {
            Some(Message::ToolResult {
                call_id,
                is_error,
                output,
            }) => {
                assert_eq!(call_id, "c2");
                assert!(is_error);
                let text = output.as_str().expect("string note");
                assert!(text.contains("interrupted by pause/restart"));
            }
            other => panic!("expected synthetic tool result, got {other:?}"),
        }
    }

    #[test]
    fn repaired_paused_log_empty_log_is_empty() {
        assert!(repaired_paused_log(&[]).is_empty());
    }
}
