//! Directory-listing filter: collapse a long listing to entry counts.

use std::collections::BTreeSet;

use super::{CompactInput, FilterPlan, ToolOutputCompactor};

/// Filter for `ls`, `find`, `tree`, and friends.
pub struct LsFilter;

impl ToolOutputCompactor for LsFilter {
    fn plan(&self, _input: &CompactInput<'_>, lines: &[&str]) -> Option<FilterPlan> {
        let entries = lines.iter().filter(|l| !l.trim().is_empty()).count();
        let dirs = lines.iter().filter(|l| is_dir_line(l)).count();
        let files = entries.saturating_sub(dirs);
        let summary = vec![format!(
            "{entries} entries: ~{dirs} dirs, ~{files} files (full listing via recall)"
        )];
        // Keep nothing verbatim; the tripwire union still surfaces error lines
        // such as "ls: cannot access ...: Permission denied".
        Some(FilterPlan {
            keep: BTreeSet::new(),
            summary,
        })
    }
}

/// Best-effort directory heuristic: a trailing `/` (ls -F / fd) or an `ls -l`
/// permission row starting with `d`. An estimate; the full listing is in the
/// stash.
fn is_dir_line(line: &str) -> bool {
    let trimmed = line.trim_end();
    trimmed.ends_with('/') || (trimmed.starts_with('d') && trimmed.contains("rw"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> CompactInput<'static> {
        CompactInput {
            content: "",
            tool_name: "bash",
            bash_command: Some("ls -la"),
            exit_code: Some(0),
            is_error: false,
        }
    }

    #[test]
    fn summarizes_entry_and_dir_counts() {
        let lines = [
            "drwxr-xr-x  2 user group 4096 src/",
            "src/",
            "main.rs",
            "lib.rs",
            "",
        ];
        let plan = LsFilter.plan(&input(), &lines).expect("applies");
        assert!(plan.keep.is_empty());
        assert_eq!(plan.summary.len(), 1);
        let summary = plan.summary.first().expect("summary line");
        assert!(summary.starts_with("4 entries:"));
        assert!(summary.contains("~2 dirs"));
        assert!(summary.contains("~2 files"));
    }
}
