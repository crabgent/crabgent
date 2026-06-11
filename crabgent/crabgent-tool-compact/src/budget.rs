//! Compute budget: cheap deterministic pre-gates that bound the work.
//!
//! Rather than a non-deterministic wall-clock timer, the budget caps the
//! input size, line count, and (for the MCP filter) JSON nesting depth.
//! Bounded input implies bounded time, so the compactor stays deterministic
//! and testable. Pathological input degrades to raw passthrough, where the
//! per-tool byte caps remain the floor.

use serde_json::Value;

use crate::config::ToolCompactConfig;

/// Whether the compactor should proceed or degrade to passthrough.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDecision {
    /// Input is within bounds; run the safety-floor and filter.
    Proceed,
    /// Input is too large/long; pass the raw output through unchanged.
    Degrade,
}

/// Cheap size/line pre-gate over the textual content.
#[must_use]
pub fn pregate(content: &str, cfg: &ToolCompactConfig) -> BudgetDecision {
    if content.len() > cfg.max_input_bytes {
        return BudgetDecision::Degrade;
    }
    let lines = content.bytes().filter(|&b| b == b'\n').count() + 1;
    if lines > cfg.max_input_lines {
        return BudgetDecision::Degrade;
    }
    BudgetDecision::Proceed
}

/// Whether a JSON value nests no deeper than `max_depth`.
///
/// Returns as soon as the cap is exceeded, so recursion is bounded by
/// `max_depth` and cannot overflow the stack on adversarial nesting.
#[must_use]
pub fn json_within_depth(value: &Value, max_depth: usize) -> bool {
    fn walk(value: &Value, max_depth: usize, current: usize) -> bool {
        if current > max_depth {
            return false;
        }
        match value {
            Value::Array(items) => items.iter().all(|v| walk(v, max_depth, current + 1)),
            Value::Object(map) => map.values().all(|v| walk(v, max_depth, current + 1)),
            _ => true,
        }
    }
    walk(value, max_depth, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn budget_proceed_vs_degrade() {
        let cfg = ToolCompactConfig::default();
        assert_eq!(pregate("small output", &cfg), BudgetDecision::Proceed);

        let huge = "x".repeat(cfg.max_input_bytes + 1);
        assert_eq!(pregate(&huge, &cfg), BudgetDecision::Degrade);
    }

    #[test]
    fn budget_degrades_on_line_count() {
        let cfg = ToolCompactConfig::default().with_min_tokens(1);
        let many = "a\n".repeat(cfg.max_input_lines + 5);
        assert_eq!(pregate(&many, &cfg), BudgetDecision::Degrade);
    }

    #[test]
    fn json_depth_cap_rejects_deep_nesting() {
        let shallow = json!({"a": {"b": [1, 2, 3]}});
        assert!(json_within_depth(&shallow, 8));

        let mut deep = json!(0);
        for _ in 0..40 {
            deep = json!([deep]);
        }
        assert!(!json_within_depth(&deep, 8));
        assert!(json_within_depth(&deep, 64));
    }
}
