//! Dual-signal success gate.
//!
//! Compaction may treat an output as a boring success only when BOTH the
//! structured status (a shell exit code, or `ToolResult.is_error`) AND a body
//! classifier agree. On conflict the verdict is [`Verdict::Uncertain`], which
//! the compactor maps to raw passthrough plus a marker. This defeats the
//! "body says 0 failures but the process exited non-zero" trap.

/// The gate's decision for a tool output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Both signals agree the call succeeded.
    Success,
    /// Both signals agree (or the structured signal alone shows) failure.
    Failure,
    /// The signals disagree; do not compact-as-success.
    Uncertain,
}

/// The structured side of the dual signal.
#[derive(Debug, Clone, Copy)]
pub struct StructuredSignal {
    /// Shell exit code when the tool is `bash`, else `None`.
    pub exit_code: Option<i32>,
    /// The `ToolResult.is_error` flag.
    pub is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Structured {
    Success,
    Failure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body {
    Success,
    Failure,
    Neutral,
}

/// Evaluate the dual-signal gate for `content` given its structured status.
#[must_use]
pub fn evaluate(signal: &StructuredSignal, content: &str) -> Verdict {
    match (classify_structured(signal), classify_body(content)) {
        (Structured::Success, Body::Success | Body::Neutral) => Verdict::Success,
        (Structured::Failure, Body::Failure | Body::Neutral) => Verdict::Failure,
        _ => Verdict::Uncertain,
    }
}

const fn classify_structured(signal: &StructuredSignal) -> Structured {
    if signal.is_error || matches!(signal.exit_code, Some(code) if code != 0) {
        Structured::Failure
    } else {
        Structured::Success
    }
}

/// Phrases that indicate the command itself failed. Deliberately specific to
/// run-summary language, not any mention of the word "error", so a successful
/// `grep error` or a source file that discusses errors does not trip the gate.
const BODY_FAILURE: &[&str] = &[
    "test result: failed",
    "tests failed",
    "test failures",
    "failures:",
    "panicked",
    "build failed",
    "compilation failed",
    "fatal error",
    "could not compile",
];

/// Phrases that indicate a clean run in the output body.
const BODY_SUCCESS: &[&str] = &[
    "test result: ok",
    "0 failed",
    "0 failures",
    "build succeeded",
    "all tests passed",
    "tests passed",
];

fn classify_body(content: &str) -> Body {
    let lower = content.to_ascii_lowercase();
    let failure = BODY_FAILURE.iter().any(|p| lower.contains(p));
    let success = BODY_SUCCESS.iter().any(|p| lower.contains(p));
    match (failure, success) {
        (true, _) => Body::Failure,
        (false, true) => Body::Success,
        (false, false) => Body::Neutral,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(exit: Option<i32>, is_error: bool) -> StructuredSignal {
        StructuredSignal {
            exit_code: exit,
            is_error,
        }
    }

    #[test]
    fn gate_success_when_both_agree() {
        let v = evaluate(&sig(Some(0), false), "test result: ok. 12 passed; 0 failed");
        assert_eq!(v, Verdict::Success);
    }

    #[test]
    fn gate_failure_when_both_agree() {
        let v = evaluate(&sig(Some(1), false), "test result: FAILED. 2 failures:");
        assert_eq!(v, Verdict::Failure);
    }

    #[test]
    fn gate_uncertain_on_conflict_exit_nonzero_body_clean() {
        // The classic trap: body says 0 failures but the process exited 1.
        let v = evaluate(&sig(Some(1), false), "summary: 0 failed, everything fine");
        assert_eq!(v, Verdict::Uncertain);
    }

    #[test]
    fn gate_uncertain_on_conflict_exit_zero_body_failure() {
        let v = evaluate(&sig(Some(0), false), "panicked while cleaning up");
        assert_eq!(v, Verdict::Uncertain);
    }

    #[test]
    fn gate_neutral_body_follows_structured() {
        assert_eq!(
            evaluate(&sig(Some(0), false), "just some neutral output"),
            Verdict::Success
        );
        assert_eq!(
            evaluate(&sig(None, true), "just some neutral output"),
            Verdict::Failure
        );
    }
}
