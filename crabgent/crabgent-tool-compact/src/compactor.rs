//! The single-owner compaction orchestrator.
//!
//! [`Compactor::run`] is pure and deterministic: it owns the whole content
//! analysis pipeline (budget, secret gate, token threshold, dual-signal gate,
//! filter selection, tripwire union, rendering) and returns a
//! [`CompactorVerdict`]. It performs no store I/O; the hook maps the verdict
//! to a `Decision` and owns the stash.

use std::collections::BTreeSet;

use crabgent_core::tokens::estimate_tokens;

use crate::budget::{self, BudgetDecision};
use crate::config::ToolCompactConfig;
use crate::filters::{CompactInput, FilterPlan, select_filter};
use crate::stats::CompactionStats;
use crate::success_gate::{self, StructuredSignal, Verdict};
use crate::tripwire::{self, TripwireHits};

/// Marker emitted when the dual-signal gate is uncertain. The hook prepends it
/// to the raw output so the model sees the uncertainty without losing data.
pub const UNCERTAIN_MARKER: &str = "<compaction-uncertain reason=\"success signals conflict\">";

/// The orchestrator's decision for one tool output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactorVerdict {
    /// Forward the raw output unchanged (too small, no matching filter, over
    /// budget, or a suspected secret leak).
    Passthrough,
    /// Forward the raw output prefixed with [`UNCERTAIN_MARKER`]; do not stash.
    UncertainMarker,
    /// Replace with the compacted body (the hook appends the coverage footer).
    Compacted {
        /// The compacted text, without the footer.
        body: String,
        /// Coverage statistics for the footer.
        stats: CompactionStats,
    },
}

/// Deterministic compaction over tool outputs.
pub struct Compactor {
    config: ToolCompactConfig,
}

impl Compactor {
    /// Build a compactor with the given tunables.
    #[must_use]
    pub const fn new(config: ToolCompactConfig) -> Self {
        Self { config }
    }

    /// Plan a verdict for one tool output. Pure and deterministic.
    #[must_use]
    pub fn run(&self, input: &CompactInput<'_>) -> CompactorVerdict {
        if budget::pregate(input.content, &self.config) == BudgetDecision::Degrade {
            return CompactorVerdict::Passthrough;
        }
        if estimate_tokens(input.content) < self.config.min_tokens {
            return CompactorVerdict::Passthrough;
        }
        if tripwire::contains_secret(input.content) {
            return CompactorVerdict::Passthrough;
        }
        // The dual-signal gate only applies to tools that carry a structured
        // success/failure notion (a shell exit code, or a soft error result).
        // For plain content tools (read_file, mcp) there is no success to
        // conflict with, so body words like "error:" must not block compaction.
        if input.exit_code.is_some() || input.is_error {
            let signal = StructuredSignal {
                exit_code: input.exit_code,
                is_error: input.is_error,
            };
            if success_gate::evaluate(&signal, input.content) == Verdict::Uncertain {
                return CompactorVerdict::UncertainMarker;
            }
        }
        let Some(filter) = select_filter(input) else {
            return CompactorVerdict::Passthrough;
        };
        let lines: Vec<&str> = input.content.lines().collect();
        let Some(plan) = filter.plan(input, &lines) else {
            return CompactorVerdict::Passthrough;
        };
        let tripwire = tripwire::scan_damage(&lines);
        let (body, stats) = render(&lines, &plan, &tripwire, input.exit_code);
        CompactorVerdict::Compacted { body, stats }
    }
}

/// Render the kept lines (filter plan unioned with the tripwire) in order,
/// inserting fold markers for omitted spans and appending summary lines.
fn render(
    lines: &[&str],
    plan: &FilterPlan,
    tripwire: &TripwireHits,
    exit_code: Option<i32>,
) -> (String, CompactionStats) {
    let mut keep: BTreeSet<usize> = plan.keep.clone();
    keep.extend(tripwire.keep.iter().copied());

    let total = lines.len();
    let mut out: Vec<String> = Vec::new();
    let mut folded_spans = 0usize;
    let mut cursor = 0usize;

    for idx in &keep {
        let idx = *idx;
        if idx > cursor {
            out.push(fold_marker(idx - cursor));
            folded_spans += 1;
        }
        if let Some(line) = lines.get(idx) {
            out.push((*line).to_owned());
        }
        cursor = idx + 1;
    }
    if cursor < total {
        out.push(fold_marker(total - cursor));
        folded_spans += 1;
    }
    out.extend(plan.summary.iter().cloned());

    let stats = CompactionStats {
        shown_lines: keep.len(),
        total_lines: total,
        folded_spans,
        kept_error_lines: tripwire.len(),
        exit_code,
    };
    (out.join("\n"), stats)
}

fn fold_marker(omitted: usize) -> String {
    format!("[... {omitted} lines omitted ...]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compactor() -> Compactor {
        Compactor::new(ToolCompactConfig::default().with_min_tokens(1))
    }

    fn read_file_input(content: &str) -> CompactInput<'_> {
        CompactInput {
            content,
            tool_name: "read_file",
            bash_command: None,
            exit_code: None,
            is_error: false,
        }
    }

    fn big_read_file(marker_line: &str) -> String {
        let mut lines: Vec<String> = (0..60).map(|i| format!("line {i}")).collect();
        lines.insert(45, marker_line.to_owned());
        lines.join("\n")
    }

    #[test]
    fn tripwire_line_present_even_if_filter_drops_it() {
        // read_file keeps head(30)+tail(10); the marker sits in the folded
        // middle, but the tripwire force-keeps it.
        let content = big_read_file("error: middle boom");
        let verdict = compactor().run(&read_file_input(&content));
        let CompactorVerdict::Compacted { body, .. } = verdict else {
            panic!("expected Compacted");
        };
        assert!(body.contains("error: middle boom"));
        assert!(body.contains("lines omitted"));
    }

    #[test]
    fn stats_counts_correct() {
        let content = big_read_file("error: boom");
        let CompactorVerdict::Compacted { stats, .. } = compactor().run(&read_file_input(&content))
        else {
            panic!("expected Compacted");
        };
        assert_eq!(stats.total_lines, 61);
        assert!(stats.kept_error_lines >= 1);
        assert!(stats.folded_spans >= 1);
        assert!(stats.shown_lines < stats.total_lines);
    }

    #[test]
    fn secret_gate_short_circuits_to_passthrough() {
        let mut content = big_read_file("ordinary line");
        content.push_str("\nexport KEY=sk-abcdef0123456789ghijkl");
        assert_eq!(
            compactor().run(&read_file_input(&content)),
            CompactorVerdict::Passthrough
        );
    }

    #[test]
    fn secret_line_never_in_compacted_body() {
        let mut content = big_read_file("ordinary line");
        content.push_str("\n-----BEGIN RSA PRIVATE KEY-----");
        // A secret forces passthrough, so there is no compacted body at all.
        assert!(matches!(
            compactor().run(&read_file_input(&content)),
            CompactorVerdict::Passthrough
        ));
    }

    #[test]
    fn verdict_passthrough_on_small_and_nomatch() {
        // small (below min_tokens with default threshold).
        let small = Compactor::new(ToolCompactConfig::default());
        assert_eq!(
            small.run(&read_file_input("tiny")),
            CompactorVerdict::Passthrough
        );
        // no matching filter (unknown tool) even when large.
        let big = "x\n".repeat(500);
        let unknown = CompactInput {
            content: &big,
            tool_name: "calendar",
            bash_command: None,
            exit_code: None,
            is_error: false,
        };
        assert_eq!(compactor().run(&unknown), CompactorVerdict::Passthrough);
    }

    #[test]
    fn uncertain_verdict_on_signal_conflict() {
        // bash, exit 1 but body says 0 failed: conflict -> uncertain marker.
        let body = format!("{}\nsummary: 0 failed, all good", "noise\n".repeat(40));
        let input = CompactInput {
            content: &body,
            tool_name: "bash",
            bash_command: Some("cargo test"),
            exit_code: Some(1),
            is_error: false,
        };
        assert_eq!(compactor().run(&input), CompactorVerdict::UncertainMarker);
    }
}
