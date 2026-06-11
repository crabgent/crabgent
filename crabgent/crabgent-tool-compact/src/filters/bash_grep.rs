//! Grep/rg filter: collapse a huge match dump to per-file counts.

use std::collections::{BTreeMap, BTreeSet};

use super::{CompactInput, FilterPlan, ToolOutputCompactor};

/// Maximum per-file count lines to emit before collapsing to "... N more".
const MAX_FILES: usize = 50;
/// Cap on distinct file buckets tracked, so output with no `path:` prefix (one
/// bucket per line) cannot grow the map unboundedly. Total stays accurate;
/// lines beyond the cap are counted but not bucketed.
const MAX_TRACKED_FILES: usize = 512;

/// Filter for `grep`, `rg`, and friends.
pub struct GrepFilter;

impl ToolOutputCompactor for GrepFilter {
    fn plan(&self, _input: &CompactInput<'_>, lines: &[&str]) -> Option<FilterPlan> {
        let mut per_file: BTreeMap<&str, usize> = BTreeMap::new();
        let mut total = 0usize;
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            total += 1;
            let file = line.split_once(':').map_or(*line, |(f, _)| f);
            if let Some(count) = per_file.get_mut(file) {
                *count += 1;
            } else if per_file.len() < MAX_TRACKED_FILES {
                per_file.insert(file, 1);
            }
        }

        let mut summary: Vec<String> = per_file
            .iter()
            .take(MAX_FILES)
            .map(|(file, count)| format!("{file}: {count} matches"))
            .collect();
        if per_file.len() > MAX_FILES {
            summary.push(format!("... and {} more files", per_file.len() - MAX_FILES));
        }
        let file_count = if per_file.len() >= MAX_TRACKED_FILES {
            format!("{MAX_TRACKED_FILES}+")
        } else {
            per_file.len().to_string()
        };
        summary.push(format!("{total} total matches across {file_count} files"));

        Some(FilterPlan {
            keep: BTreeSet::new(),
            summary,
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
            bash_command: Some("rg pattern"),
            exit_code: Some(0),
            is_error: false,
        }
    }

    #[test]
    fn dedups_into_per_file_counts() {
        let lines = [
            "src/a.rs:10:match one",
            "src/a.rs:20:match two",
            "src/b.rs:5:match three",
        ];
        let plan = GrepFilter.plan(&input(), &lines).expect("applies");
        assert!(plan.keep.is_empty());
        assert!(plan.summary.iter().any(|s| s == "src/a.rs: 2 matches"));
        assert!(plan.summary.iter().any(|s| s == "src/b.rs: 1 matches"));
        assert!(
            plan.summary
                .iter()
                .any(|s| s == "3 total matches across 2 files")
        );
    }
}
