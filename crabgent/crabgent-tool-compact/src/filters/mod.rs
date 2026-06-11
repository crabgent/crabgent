//! Name-keyed semantic filters.
//!
//! Each filter is a deterministic, separately tested unit implementing
//! [`ToolOutputCompactor`]. The dispatch key is the kernel-stamped tool name
//! plus, for `bash`, the subcommand sniffed from the command string in
//! `call.args`. The subcommand is NEVER read from a banner inside the output
//! payload: a malicious or proxied payload must not be able to pick its own
//! filter.
//!
//! A filter returns a [`FilterPlan`] over the pre-split lines: which lines to
//! keep verbatim plus synthesized summary lines. The compactor unions the
//! kept set with the tripwire's force-kept lines and renders the result, so a
//! filter can never drop a diagnostic line.

use std::collections::BTreeSet;

pub mod bash_git;
pub mod bash_grep;
pub mod bash_ls;
pub mod bash_test_runner;
pub mod mcp;
pub mod read_file;

use bash_git::GitFilter;
use bash_grep::GrepFilter;
use bash_ls::LsFilter;
use bash_test_runner::TestRunnerFilter;
use mcp::McpFilter;
use read_file::ReadFileFilter;

/// The structured input a filter (and the dual-signal gate) reasons over.
#[derive(Debug, Clone, Copy)]
pub struct CompactInput<'a> {
    /// The full textual output of the tool.
    pub content: &'a str,
    /// The kernel-stamped tool name (routing key).
    pub tool_name: &'a str,
    /// The `bash` command string from `call.args`, when the tool is `bash`.
    pub bash_command: Option<&'a str>,
    /// The shell exit code, when the tool is `bash`.
    pub exit_code: Option<i32>,
    /// The `ToolResult.is_error` flag.
    pub is_error: bool,
}

/// A filter's plan for one output: verbatim line indices plus summaries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterPlan {
    /// Indices into the original line slice to keep verbatim.
    pub keep: BTreeSet<usize>,
    /// Synthesized summary lines appended after the kept content.
    pub summary: Vec<String>,
}

/// A deterministic reduction over one tool output.
pub trait ToolOutputCompactor: Send + Sync {
    /// Plan a reduction over the pre-split `lines`, or `None` if this filter
    /// does not apply (the caller then passes the output through unchanged).
    fn plan(&self, input: &CompactInput<'_>, lines: &[&str]) -> Option<FilterPlan>;
}

/// The bash subcommand families v1 recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashSub {
    /// `cargo test`, `pytest`, `go test`, ...
    TestRunner,
    /// `git ...`
    Git,
    /// `ls`, `find`, `tree`, ...
    Ls,
    /// `grep`, `rg`, ... (direct or piped)
    Grep,
    /// Anything else: no compaction.
    Other,
}

/// Select the matching filter, or `None` for a passthrough (unknown command).
#[must_use]
pub fn select_filter(input: &CompactInput<'_>) -> Option<&'static dyn ToolOutputCompactor> {
    match input.tool_name {
        "bash" => bash_filter(input.bash_command?),
        "read_file" => Some(&ReadFileFilter),
        name if name.contains("__") => Some(&McpFilter),
        _ => None,
    }
}

fn bash_filter(command: &str) -> Option<&'static dyn ToolOutputCompactor> {
    match bash_subcommand(command) {
        BashSub::TestRunner => Some(&TestRunnerFilter),
        BashSub::Git => Some(&GitFilter),
        BashSub::Ls => Some(&LsFilter),
        BashSub::Grep => Some(&GrepFilter),
        BashSub::Other => None,
    }
}

/// Signatures that may appear anywhere in a (possibly piped) command line.
const TEST_RUNNER_SIGNATURES: &[&str] = &[
    "cargo test",
    "cargo nextest",
    "pytest",
    "py.test",
    "go test",
    "npm test",
    "npm run test",
    "vitest",
    "jest",
    "phpunit",
    "rspec",
    "ctest",
];

/// Classify a bash command string by its subcommand. Reads only the command,
/// never the output.
#[must_use]
pub fn bash_subcommand(command: &str) -> BashSub {
    let lower = command.to_ascii_lowercase();
    if TEST_RUNNER_SIGNATURES.iter().any(|s| lower.contains(s)) {
        return BashSub::TestRunner;
    }
    if let Some(head) = first_command_word(command) {
        match head {
            "git" => return BashSub::Git,
            "ls" | "ll" | "dir" | "tree" | "find" | "fd" | "exa" | "eza" => return BashSub::Ls,
            "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" => return BashSub::Grep,
            _ => {}
        }
    }
    if ["| grep", "|grep", "| rg", "|rg", "| egrep", "| ag"]
        .iter()
        .any(|p| lower.contains(p))
    {
        return BashSub::Grep;
    }
    BashSub::Other
}

/// The first real command word, skipping `sudo`, `env`, and `VAR=value`
/// prefixes, with any leading path stripped.
fn first_command_word(command: &str) -> Option<&str> {
    command
        .split_whitespace()
        .find(|tok| !is_wrapper(tok) && !is_env_assignment(tok))
        .map(basename)
}

fn is_wrapper(tok: &str) -> bool {
    matches!(tok, "sudo" | "env" | "command" | "nice" | "time" | "exec")
}

fn is_env_assignment(tok: &str) -> bool {
    matches!(tok.split_once('='), Some((name, _))
        if !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
}

fn basename(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

/// Keep every line containing any of the lowercase `markers`.
#[must_use]
pub(crate) fn keep_lines_matching(lines: &[&str], markers: &[&str]) -> BTreeSet<usize> {
    let mut keep = BTreeSet::new();
    for (idx, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        if markers.iter().any(|m| lower.contains(m)) {
            keep.insert(idx);
        }
    }
    keep
}

/// Keep the first `head` and last `tail` lines.
#[must_use]
pub(crate) fn head_tail_indices(total: usize, head: usize, tail: usize) -> BTreeSet<usize> {
    let mut keep: BTreeSet<usize> = (0..head.min(total)).collect();
    if total > tail {
        keep.extend((total - tail)..total);
    } else {
        keep.extend(0..total);
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash(command: &str) -> CompactInput<'_> {
        CompactInput {
            content: "",
            tool_name: "bash",
            bash_command: Some(command),
            exit_code: Some(0),
            is_error: false,
        }
    }

    #[test]
    fn subcommand_classification() {
        assert_eq!(bash_subcommand("cargo test --all"), BashSub::TestRunner);
        assert_eq!(
            bash_subcommand("RUST_LOG=info cargo nextest run"),
            BashSub::TestRunner
        );
        assert_eq!(bash_subcommand("git push origin main"), BashSub::Git);
        assert_eq!(bash_subcommand("/usr/bin/ls -la"), BashSub::Ls);
        assert_eq!(bash_subcommand("rg pattern src/"), BashSub::Grep);
        assert_eq!(bash_subcommand("cat log.txt | grep ERROR"), BashSub::Grep);
        assert_eq!(bash_subcommand("sudo cat /etc/hosts"), BashSub::Other);
    }

    #[test]
    fn subcommand_sniffed_from_args_not_output_banner() {
        // The command is a harmless cat; a banner inside the OUTPUT claiming
        // to be `cargo test` is irrelevant because select_filter never sees
        // the content for dispatch.
        let input = bash("cat results.txt");
        assert!(select_filter(&input).is_none());
        // The same tool with an actual test command does match.
        assert!(select_filter(&bash("cargo test")).is_some());
    }

    #[test]
    fn dispatch_routes_by_tool_name() {
        let read = CompactInput {
            content: "",
            tool_name: "read_file",
            bash_command: None,
            exit_code: None,
            is_error: false,
        };
        assert!(select_filter(&read).is_some());

        let mcp = CompactInput {
            tool_name: "github__search",
            ..read
        };
        assert!(select_filter(&mcp).is_some());

        let unknown = CompactInput {
            tool_name: "calendar",
            ..read
        };
        assert!(select_filter(&unknown).is_none());
    }

    #[test]
    fn head_tail_handles_small_and_large() {
        assert_eq!(head_tail_indices(3, 5, 5), BTreeSet::from([0, 1, 2]));
        assert_eq!(head_tail_indices(10, 2, 2), BTreeSet::from([0, 1, 8, 9]));
    }
}
