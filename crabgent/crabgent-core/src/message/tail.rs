//! Shared message-tail classifiers.
//!
//! Hooks that insert or compact messages must agree on where an unresolved
//! toolchain tail starts. This module keeps that parser contract in core so
//! inject and compact hooks cannot drift apart.

use serde_json::Value;

use crate::message::Message;

/// `true` when a serialized message belongs to the unresolved provider tail.
#[must_use]
pub fn is_unresolved_tail_value(value: &Value) -> bool {
    let role = value.get("role").and_then(Value::as_str).unwrap_or("");
    match role {
        "tool_result" => true,
        "assistant" => {
            let tool_calls_empty = value
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty);
            if !tool_calls_empty {
                return true;
            }
            value
                .get("text")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        }
        _ => false,
    }
}

/// Boundary between settled serialized history and unresolved tail.
#[must_use]
pub fn unresolved_tail_boundary(messages: &[Value]) -> usize {
    messages
        .iter()
        .rposition(|message| !is_unresolved_tail_value(message))
        .map_or(0, |pos| pos + 1)
}

/// Move a proposed boundary left so assistant tool calls and their trailing
/// tool results stay together.
#[must_use]
pub fn tool_result_group_boundary(messages: &[Message], proposed: usize) -> usize {
    if !matches!(messages.get(proposed), Some(Message::ToolResult { .. })) {
        return proposed;
    }
    let mut idx = proposed;
    while idx > 0 && matches!(messages.get(idx), Some(Message::ToolResult { .. })) {
        idx -= 1;
    }
    match messages.get(idx) {
        Some(Message::Assistant { tool_calls, .. }) if !tool_calls.is_empty() => idx,
        _ => proposed,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ToolCall;

    fn tool_call() -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "lookup".into(),
            args: json!({}),
            thought_signature: None,
        }
    }

    #[test]
    fn unresolved_tail_value_detects_tool_plumbing() {
        assert!(is_unresolved_tail_value(
            &json!({"role": "assistant", "text": "", "tool_calls": [tool_call()]})
        ));
        assert!(is_unresolved_tail_value(
            &json!({"role": "tool_result", "call_id": "call-1"})
        ));
        assert!(is_unresolved_tail_value(
            &json!({"role": "assistant", "text": "", "tool_calls": []})
        ));
        assert!(!is_unresolved_tail_value(
            &json!({"role": "assistant", "text": "done", "tool_calls": []})
        ));
    }

    #[test]
    fn unresolved_tail_boundary_finds_settled_prefix_end() {
        let messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "text": "", "tool_calls": [tool_call()]}),
            json!({"role": "tool_result", "call_id": "call-1"}),
        ];

        assert_eq!(unresolved_tail_boundary(&messages), 1);
    }

    #[test]
    fn tool_result_group_boundary_keeps_assistant_with_results() {
        let messages = vec![
            Message::User {
                content: vec![],
                timestamp: None,
            },
            Message::Assistant {
                text: String::new(),
                tool_calls: vec![tool_call()],
            },
            Message::ToolResult {
                call_id: "call-1".into(),
                output: json!({"ok": true}),
                is_error: false,
            },
        ];

        assert_eq!(tool_result_group_boundary(&messages, 2), 1);
    }
}
