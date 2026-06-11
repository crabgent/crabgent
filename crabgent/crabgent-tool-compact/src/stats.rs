//! Coverage statistics for a compacted block, rendered into the footer.

/// What a compaction kept and folded, for the machine-readable footer.
///
/// Honest by construction: the model sees the shape of what is missing and
/// how much was retained verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompactionStats {
    /// Lines present in the compacted body.
    pub shown_lines: usize,
    /// Lines in the original output.
    pub total_lines: usize,
    /// Number of folded (omitted) spans.
    pub folded_spans: usize,
    /// Lines force-kept by the tripwire (errors, panics, denials).
    pub kept_error_lines: usize,
    /// Structured exit code when the source is a shell command.
    pub exit_code: Option<i32>,
}

impl CompactionStats {
    /// Lines omitted from the compacted body.
    #[must_use]
    pub const fn omitted_lines(&self) -> usize {
        self.total_lines.saturating_sub(self.shown_lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_is_total_minus_shown() {
        let stats = CompactionStats {
            shown_lines: 40,
            total_lines: 1200,
            folded_spans: 3,
            kept_error_lines: 2,
            exit_code: Some(1),
        };
        assert_eq!(stats.omitted_lines(), 1160);
    }

    #[test]
    fn omitted_saturates_when_shown_exceeds_total() {
        let stats = CompactionStats {
            shown_lines: 5,
            total_lines: 2,
            ..CompactionStats::default()
        };
        assert_eq!(stats.omitted_lines(), 0);
    }
}
