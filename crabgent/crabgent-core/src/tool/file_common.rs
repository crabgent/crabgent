//! Shared helpers for the builtin file tools (`read_file`, `update_file`).
//!
//! Path validation against a configured root lives in [`super::path_root`];
//! this module holds the soft-error mapping the file tools share at the
//! `execute_result` boundary.

use serde_json::{Value, json};

use crate::error::ToolError;
use crate::types::ToolResult;

/// Map a file-tool `execute` result onto the run loop's soft/hard boundary.
///
/// `NotFound` (missing file, missing anchor) and `Io` (read/write failure,
/// non-UTF-8 content, mid-stream disk error) become soft errors so the run
/// loop feeds the failure back to the LLM as a tool result rather than
/// aborting: the model can retry with a different path, widen an anchor, or
/// give up gracefully.
///
/// Hard variants (`Permission`, `Cancelled`, anything outside this match)
/// keep propagating so the path-traversal guard and cancellation stay
/// authoritative. `InvalidArgs` also stays hard: an ambiguous anchor is a
/// caller error, not an LLM-recoverable condition.
pub(super) fn soft_file_error_or(
    result: Result<Value, ToolError>,
) -> Result<ToolResult, ToolError> {
    match result {
        Ok(value) => Ok(ToolResult::success(value)),
        Err(ToolError::NotFound(msg) | ToolError::Io(msg)) => {
            Ok(ToolResult::soft_error(json!({ "error": msg })))
        }
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_maps_to_success_result() {
        let result = soft_file_error_or(Ok(json!({"ok": true}))).expect("ok");

        assert!(!result.is_error);
        assert_eq!(result.output, json!({"ok": true}));
    }

    #[test]
    fn not_found_maps_to_soft_error() {
        let result =
            soft_file_error_or(Err(ToolError::NotFound("missing".to_owned()))).expect("soft");

        assert!(result.is_error);
        assert_eq!(result.output, json!({"error": "missing"}));
    }

    #[test]
    fn io_maps_to_soft_error() {
        let result = soft_file_error_or(Err(ToolError::Io("disk".to_owned()))).expect("soft");

        assert!(result.is_error);
        assert_eq!(result.output, json!({"error": "disk"}));
    }

    #[test]
    fn permission_stays_hard() {
        let err =
            soft_file_error_or(Err(ToolError::Permission("denied".to_owned()))).expect_err("hard");

        assert!(matches!(err, ToolError::Permission(reason) if reason == "denied"));
    }

    #[test]
    fn invalid_args_stays_hard() {
        let err = soft_file_error_or(Err(ToolError::InvalidArgs("ambiguous".to_owned())))
            .expect_err("hard");

        assert!(matches!(err, ToolError::InvalidArgs(reason) if reason == "ambiguous"));
    }
}
