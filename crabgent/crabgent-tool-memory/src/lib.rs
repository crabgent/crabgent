//! # crabgent-tool-memory
//!
//! [`MemoryTool`] exposes long-term memory CRUD to the LLM as a single
//! tool with op-arg dispatch (`search`/`store`/`get`/`delete`). Separate
//! lifecycle tools archive, unarchive, extend expiry, and forget documents.
//!
//! The tool is policy-gated per operation via typed `Action` variants:
//! the policy hook receives `Action::MemorySearch`, `Action::MemoryStore`,
//! `Action::MemoryGet`, or `Action::MemoryDelete` carrying the requested
//! [`MemoryScope`] so it can enforce per-subject, per-scope rules.
//! `AllowAllPolicy` lets every operation through;
//! `StrictPolicy` (in `crabgent-core::policy::strict`) supports per-action
//! allow rules with attribute conditions.
//!
//! Scope is required: callers must include a `scope` field in their
//! tool-args; otherwise the tool returns `ToolError::InvalidArgs`.
//!
//! [`MemoryScope`]: crabgent_core::MemoryScope
//! [`MemoryId`]: crabgent_core::MemoryId

#![forbid(unsafe_code)]

pub mod tool;

mod lifecycle;
mod ops;

pub use lifecycle::{ArchiveTool, ExtendExpiryTool, ForgetTool, UnarchiveTool};
pub use tool::MemoryTool;

use crabgent_core::error::ToolError;
use crabgent_store::StoreError;

/// Map a `StoreError` to an opaque `ToolError::Execution` carrying only the op
/// label (no `Display` of the underlying error). Sites that propagate store
/// failures to the LLM must use this helper. Backend detail is intentionally
/// discarded before building the LLM-visible `"<op>: backend unavailable"`
/// message.
///
/// The convention here mirrors `crabgent-tool-task`'s `store_unavailable` helper
/// (see `crabgent-tool-task/src/tool.rs`). Op label is passed in pre-formatted
/// so both dot-form (`memory.search`) and underscore-form (`memory_archive`)
/// stay consistent with the pre-existing logging convention per call site.
pub(crate) fn store_unavailable(op: &str, err: &StoreError) -> ToolError {
    crabgent_log::warn!(
        op = %op,
        error_kind = err.kind(),
        transient = err.is_transient(),
        "memory store unavailable",
    );
    ToolError::backend_unavailable(op, err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_unavailable_emits_opaque_execution_with_op_label() {
        let err = StoreError::Conflict("internal: secret-token leak".into());
        let mapped = store_unavailable("memory.search", &err);
        match mapped {
            ToolError::Execution(msg) => {
                assert_eq!(msg, "memory.search: backend unavailable");
                assert!(
                    !msg.contains("secret-token"),
                    "underlying StoreError Display must NOT leak into LLM message"
                );
                assert!(
                    !msg.contains("conflict"),
                    "underlying StoreError variant must NOT leak into LLM message"
                );
            }
            other => panic!("expected Execution, got {other:?}"),
        }
    }

    #[test]
    fn store_unavailable_preserves_dot_op_label() {
        let err = StoreError::NotFound;
        let mapped = store_unavailable("memory.delete.get", &err);
        let ToolError::Execution(msg) = mapped else {
            panic!("expected Execution");
        };
        assert!(msg.starts_with("memory.delete.get:"));
    }

    #[test]
    fn store_unavailable_preserves_underscore_op_label() {
        let err = StoreError::NotFound;
        let mapped = store_unavailable("memory_archive", &err);
        let ToolError::Execution(msg) = mapped else {
            panic!("expected Execution");
        };
        assert!(msg.starts_with("memory_archive:"));
    }
}
