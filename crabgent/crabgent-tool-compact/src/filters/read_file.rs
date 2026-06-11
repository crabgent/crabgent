//! `read_file` filter: keep head + tail, fold the middle.

use super::{CompactInput, FilterPlan, ToolOutputCompactor, head_tail_indices};

/// Lines kept from the start of the file.
const HEAD: usize = 30;
/// Lines kept from the end of the file.
const TAIL: usize = 10;

/// Filter for the `read_file` tool. Smarter than the dumb byte cut: it keeps
/// the head and tail so structure (imports, signatures, trailing summary)
/// survives, and the compactor's fold marker records the omitted line count.
pub struct ReadFileFilter;

impl ToolOutputCompactor for ReadFileFilter {
    fn plan(&self, _input: &CompactInput<'_>, lines: &[&str]) -> Option<FilterPlan> {
        Some(FilterPlan {
            keep: head_tail_indices(lines.len(), HEAD, TAIL),
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
            tool_name: "read_file",
            bash_command: None,
            exit_code: None,
            is_error: false,
        }
    }

    #[test]
    fn keeps_head_and_tail_folds_middle() {
        let owned: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let plan = ReadFileFilter.plan(&input(), &lines).expect("applies");
        assert!(plan.keep.contains(&0));
        assert!(plan.keep.contains(&29));
        assert!(plan.keep.contains(&99));
        // a middle line is folded.
        assert!(!plan.keep.contains(&50));
        assert_eq!(plan.keep.len(), HEAD + TAIL);
    }

    #[test]
    fn small_file_keeps_everything() {
        let lines = ["a", "b", "c"];
        let plan = ReadFileFilter.plan(&input(), &lines).expect("applies");
        assert_eq!(plan.keep.len(), 3);
    }
}
