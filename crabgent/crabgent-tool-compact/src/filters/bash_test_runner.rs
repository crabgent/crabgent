//! Test-runner filter: keep failure lines and the summary, drop pass spam.

use super::{
    CompactInput, FilterPlan, ToolOutputCompactor, head_tail_indices, keep_lines_matching,
};

/// Lines worth keeping from a test-runner output. The per-test "... ok" /
/// "PASS [..]" spam matches none of these and is folded. Failure and
/// diagnostic lines are also retained by the tripwire union.
const KEEP: &[&str] = &[
    "test result:",
    "failures:",
    "summary",
    "error[",
    "panicked",
    "failed",
    "passed",
    "running ",
    "warning:",
];

/// Filter for `cargo test`, `pytest`, `go test`, and friends.
pub struct TestRunnerFilter;

impl ToolOutputCompactor for TestRunnerFilter {
    fn plan(&self, _input: &CompactInput<'_>, lines: &[&str]) -> Option<FilterPlan> {
        let mut keep = keep_lines_matching(lines, KEEP);
        if keep.is_empty() {
            // No recognizable summary line: fall back to the tail.
            keep = head_tail_indices(lines.len(), 0, 5);
        }
        Some(FilterPlan {
            keep,
            summary: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> CompactInput<'static> {
        CompactInput {
            content: "",
            tool_name: "bash",
            bash_command: Some("cargo test"),
            exit_code: Some(0),
            is_error: false,
        }
    }

    #[test]
    fn keeps_failures_and_summary_drops_pass_spam() {
        let lines = [
            "test foo ... ok",
            "test bar ... ok",
            "test baz ... FAILED",
            "test qux ... ok",
            "failures:",
            "    baz",
            "test result: FAILED. 3 passed; 1 failed",
        ];
        let plan = TestRunnerFilter.plan(&input(), &lines).expect("applies");
        // pass-spam lines 0,1,3 dropped.
        assert!(!plan.keep.contains(&0));
        assert!(!plan.keep.contains(&1));
        assert!(!plan.keep.contains(&3));
        // FAILED line, failures header, result line kept.
        assert!(plan.keep.contains(&2));
        assert!(plan.keep.contains(&4));
        assert!(plan.keep.contains(&6));
    }

    #[test]
    fn passing_run_keeps_result_line() {
        let lines = ["test a ... ok", "test result: ok. 12 passed; 0 failed"];
        let plan = TestRunnerFilter.plan(&input(), &lines).expect("applies");
        assert!(plan.keep.contains(&1));
    }

    #[test]
    fn falls_back_to_tail_when_no_markers() {
        let lines = ["noise", "more noise", "tail line"];
        let plan = TestRunnerFilter.plan(&input(), &lines).expect("applies");
        assert!(plan.keep.contains(&2));
    }
}
