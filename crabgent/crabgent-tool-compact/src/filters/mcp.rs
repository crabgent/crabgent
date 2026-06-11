//! MCP filter: conservative size-gate for arbitrary proxied output.
//!
//! An MCP tool's output shape is unknown, so this filter is cautious: it
//! keeps a head + tail of the raw text and, when the payload parses as JSON
//! within the depth cap, appends a top-level key summary. It never trusts a
//! banner inside the payload (dispatch already happened on the tool name).

use super::{CompactInput, FilterPlan, ToolOutputCompactor, head_tail_indices};
use crate::budget::json_within_depth;
use crate::config::DEFAULT_MAX_JSON_DEPTH;

/// Lines kept from the start of the payload.
const HEAD: usize = 20;
/// Lines kept from the end of the payload.
const TAIL: usize = 10;
/// Maximum top-level keys listed in the summary.
const MAX_KEYS: usize = 40;

/// Filter for `{server}__{tool}` MCP outputs.
pub struct McpFilter;

impl ToolOutputCompactor for McpFilter {
    fn plan(&self, input: &CompactInput<'_>, lines: &[&str]) -> Option<FilterPlan> {
        let keep = head_tail_indices(lines.len(), HEAD, TAIL);
        let summary = json_key_summary(input.content);
        Some(FilterPlan { keep, summary })
    }
}

/// Summarize a JSON object's top-level keys, depth-capped to bound the walk.
fn json_key_summary(content: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return Vec::new();
    };
    if !json_within_depth(&value, DEFAULT_MAX_JSON_DEPTH) {
        return Vec::new();
    }
    match value {
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().take(MAX_KEYS).cloned().collect();
            if keys.is_empty() {
                Vec::new()
            } else {
                vec![format!("json keys: {}", keys.join(", "))]
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mcp_input(content: &str) -> CompactInput<'_> {
        CompactInput {
            content,
            tool_name: "github__search",
            bash_command: None,
            exit_code: None,
            is_error: false,
        }
    }

    #[test]
    fn mcp_size_gate_key_summary_depth_capped() {
        let content = r#"{"items": [1, 2, 3], "total_count": 3, "incomplete": false}"#;
        let plan = McpFilter
            .plan(&mcp_input(content), &[content])
            .expect("applies");
        assert_eq!(plan.summary.len(), 1);
        let summary = plan.summary.first().expect("summary");
        assert!(summary.contains("items"));
        assert!(summary.contains("total_count"));
    }

    #[test]
    fn non_object_json_yields_no_key_summary() {
        let content = "[1, 2, 3]";
        let plan = McpFilter
            .plan(&mcp_input(content), &[content])
            .expect("applies");
        assert!(plan.summary.is_empty());
    }

    #[test]
    fn non_json_payload_keeps_head_tail_without_summary() {
        let owned: Vec<String> = (0..60).map(|i| format!("row {i}")).collect();
        let lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let plan = McpFilter
            .plan(&mcp_input("not json at all"), &lines)
            .expect("applies");
        assert!(plan.summary.is_empty());
        assert!(plan.keep.contains(&0));
        assert!(plan.keep.contains(&59));
    }
}
