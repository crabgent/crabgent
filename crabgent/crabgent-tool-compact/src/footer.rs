//! The mandatory machine-readable coverage footer for a compacted block.

use crate::handle::RecallHandle;
use crate::stats::CompactionStats;

/// Render the coverage footer appended to every compacted block.
///
/// Example:
/// `[compacted: shown 40/1200 lines, 3 spans folded, 2 error-lines kept, exit=1, recall: <handle>]`
#[must_use]
pub fn render_footer(stats: &CompactionStats, handle: &RecallHandle) -> String {
    let exit = stats
        .exit_code
        .map_or_else(|| "n/a".to_owned(), |code| code.to_string());
    format!(
        "[compacted: shown {shown}/{total} lines, {folded} spans folded, \
         {errors} error-lines kept, exit={exit}, recall: {handle}]",
        shown = stats.shown_lines,
        total = stats.total_lines,
        folded = stats.folded_spans,
        errors = stats.kept_error_lines,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::run_id::RunId;

    fn handle() -> RecallHandle {
        RecallHandle::new(&RunId::new(), "payload")
    }

    #[test]
    fn footer_renders_machine_readable() {
        let stats = CompactionStats {
            shown_lines: 40,
            total_lines: 1200,
            folded_spans: 3,
            kept_error_lines: 2,
            exit_code: Some(1),
        };
        let h = handle();
        let footer = render_footer(&stats, &h);
        assert!(footer.starts_with("[compacted: shown 40/1200 lines,"));
        assert!(footer.contains("3 spans folded"));
        assert!(footer.contains("2 error-lines kept"));
        assert!(footer.contains("exit=1"));
        assert!(footer.contains(&format!("recall: {h}")));
        assert!(footer.ends_with(']'));
    }

    #[test]
    fn footer_renders_exit_na_when_absent() {
        let stats = CompactionStats {
            shown_lines: 1,
            total_lines: 2,
            ..CompactionStats::default()
        };
        let footer = render_footer(&stats, &handle());
        assert!(footer.contains("exit=n/a"));
    }
}
