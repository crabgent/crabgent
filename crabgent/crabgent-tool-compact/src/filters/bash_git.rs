//! Git filter: keep status/result headers, fold the per-file lines.

use super::{
    CompactInput, FilterPlan, ToolOutputCompactor, head_tail_indices, keep_lines_matching,
};

/// Header and result markers from `git status` / `git push` / `git commit`.
/// Per-file path lines match none of these and are folded.
const KEEP: &[&str] = &[
    "on branch",
    "your branch",
    "head is now",
    "head detached",
    "changes to be committed",
    "changes not staged",
    "untracked files",
    "nothing to commit",
    "file changed",
    "files changed",
    "insertion",
    "deletion",
    "->",
    "[new branch]",
    "[rejected]",
    "to github.com",
    "to git@",
    "writing objects",
    "remote:",
    "merge made",
    "fast-forward",
];

/// Filter for `git ...` output.
pub struct GitFilter;

impl ToolOutputCompactor for GitFilter {
    fn plan(&self, _input: &CompactInput<'_>, lines: &[&str]) -> Option<FilterPlan> {
        let mut keep = keep_lines_matching(lines, KEEP);
        if keep.is_empty() {
            keep = head_tail_indices(lines.len(), 2, 2);
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
            bash_command: Some("git status"),
            exit_code: Some(0),
            is_error: false,
        }
    }

    #[test]
    fn keeps_headers_folds_file_lines() {
        let lines = [
            "On branch main",
            "Untracked files:",
            "  src/a.rs",
            "  src/b.rs",
            "  src/c.rs",
            "nothing added to commit",
        ];
        let plan = GitFilter.plan(&input(), &lines).expect("applies");
        assert!(plan.keep.contains(&0));
        assert!(plan.keep.contains(&1));
        // file lines folded.
        assert!(!plan.keep.contains(&2));
        assert!(!plan.keep.contains(&3));
        assert!(!plan.keep.contains(&4));
    }

    #[test]
    fn keeps_push_result_lines() {
        let lines = [
            "Enumerating objects: 5, done.",
            "Writing objects: 100% (3/3), done.",
            "To github.com:owner/repo.git",
            "   abc123..def456  main -> main",
        ];
        let plan = GitFilter.plan(&input(), &lines).expect("applies");
        assert!(plan.keep.contains(&1));
        assert!(plan.keep.contains(&2));
        assert!(plan.keep.contains(&3));
    }
}
