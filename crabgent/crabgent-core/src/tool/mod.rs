//! Tool trait, minimal execution context, and the four built-in tools.

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::action::Action;
use crate::error::ToolError;
use crate::model::{ResolvedEffort, ResolvedModelWithSource};
use crate::policy::{PolicyDecision, PolicyHook};
use crate::subject::Subject;
use crate::types::ToolResult;

pub mod bash;
mod file_common;
mod path_root;
pub mod read_file;
pub mod update_file;
pub mod write_file;

pub use bash::BashTool;
pub use read_file::ReadFileTool;
pub use update_file::UpdateFileTool;
pub use write_file::WriteFileTool;

pub(super) const TRUNCATE_MARKER: &str = "\n... [truncated]";

/// Shared by builtin tools that cap byte streams before exposing text.
pub(super) fn safe_truncate(bytes: &[u8], cap: usize) -> String {
    let head = bytes.get(..bytes.len().min(cap)).unwrap_or(bytes);
    if let Ok(text) = std::str::from_utf8(head) {
        crate::text::truncate_bytes_at_boundary(text, cap).to_owned()
    } else {
        let lossy = String::from_utf8_lossy(head);
        crate::text::truncate_bytes_at_boundary(&lossy, cap).to_owned()
    }
}

pub fn parse_args<T>(args: Value) -> Result<T, ToolError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(args).map_err(|err| ToolError::InvalidArgs(err.to_string()))
}

pub fn parse_args_with_context<T>(args: Value, context: &str) -> Result<T, ToolError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(args).map_err(|err| ToolError::InvalidArgs(format!("{context}: {err}")))
}

pub fn clamp_positive_limit(limit: u32, max: u32, context: &str) -> Result<u32, ToolError> {
    if limit == 0 {
        return Err(ToolError::InvalidArgs(format!(
            "{context}: limit must be at least 1"
        )));
    }
    Ok(limit.min(max))
}

pub fn clamp_optional_usize_limit(limit: Option<u64>, default: usize, max: usize) -> usize {
    limit
        .and_then(|limit| usize::try_from(limit).ok())
        .unwrap_or(default)
        .min(max)
}

pub trait ToolOp: Copy {
    const JSON_VALUES: &'static [&'static str];

    fn as_str(self) -> &'static str;
}

pub fn op_schema<O: ToolOp>(description: &'static str) -> Value {
    json!({
        "type": "string",
        "enum": O::JSON_VALUES,
        "description": description,
    })
}

/// JSON Schema for a [`crate::memory::MemoryScope`] selector argument.
///
/// All five fields are optional and nullable (`["string", "null"]`); tools
/// that accept a memory scope reuse this shape instead of re-declaring it.
#[must_use]
pub fn memory_scope_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "owner": {"type": ["string", "null"]},
            "channel": {"type": ["string", "null"]},
            "conv": {"type": ["string", "null"]},
            "agent": {"type": ["string", "null"]},
            "kind": {"type": ["string", "null"]}
        }
    })
}

pub fn soft_error_object(reason: impl Into<String>) -> ToolResult {
    ToolResult::soft_error(json!({ "error": reason.into() }))
}

pub async fn gate_tool_action(
    policy: &dyn PolicyHook,
    ctx: &ToolCtx,
    action: &Action,
) -> Result<(), ToolError> {
    match policy.allow(&ctx.subject, action).await {
        PolicyDecision::Allow => Ok(()),
        PolicyDecision::Deny(reason) => Err(ToolError::Permission(reason)),
    }
}

/// Minimal context passed to a tool's `execute`.
///
/// Intentionally narrow: the tool receives subject identity, optional
/// cancellation, current model/effort metadata, and an optional session id.
/// Tools must not reach the hook registry, the injection registry (when
/// one is configured), or other tools' state from here.
///
/// `session_id` carries the id of the persisted session backing the
/// current run, when one is known (resolved by a session-persisting hook
/// during `on_session_start`). Tools that operate against that session
/// (e.g. `models.set_session`) may use it as a fallback when the LLM
/// caller omits the argument, since LLM agents cannot introspect their
/// own session id. The value is an opaque string (`SessionId::to_string`)
/// to avoid coupling `crabgent-core` to the `crabgent-store` id types.
pub struct ToolCtx {
    pub subject: Subject,
    pub cancel: Option<CancellationToken>,
    pub current_model: Option<ResolvedModelWithSource>,
    pub current_effort: Option<ResolvedEffort>,
    pub session_id: Option<String>,
}

impl ToolCtx {
    pub const fn new(subject: Subject) -> Self {
        Self {
            subject,
            cancel: None,
            current_model: None,
            current_effort: None,
            session_id: None,
        }
    }

    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    #[must_use]
    pub fn with_current_model(mut self, current_model: ResolvedModelWithSource) -> Self {
        self.current_model = Some(current_model);
        self
    }

    #[must_use]
    pub const fn with_current_effort(mut self, current_effort: ResolvedEffort) -> Self {
        self.current_effort = Some(current_effort);
        self
    }

    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }
}

/// A tool that the kernel can dispatch.
///
/// Args are loose JSON; implementors deserialise via `serde::Deserialize`
/// on a typed args struct. The kernel does not validate args.
///
/// Output is loose JSON; implementors are responsible for capping output
/// size. The kernel does not truncate.
///
/// `execute` is the simple success path. Return `Err(ToolError)` for hard
/// failures that should stop the run. The run loop maps `InvalidArgs`,
/// `NotFound`, and `Permission` into soft tool results that the LLM can repair
/// in a follow-up tool call. Override `execute_result` only for custom
/// recoverable shapes that do not fit those variants.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters_schema(&self) -> Value;
    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError>;

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        self.execute(args, ctx).await.map(ToolResult::success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[test]
    fn memory_scope_schema_matches_literal() {
        assert_eq!(
            memory_scope_schema(),
            json!({
                "type": "object",
                "properties": {
                    "owner": {"type": ["string", "null"]},
                    "channel": {"type": ["string", "null"]},
                    "conv": {"type": ["string", "null"]},
                    "agent": {"type": ["string", "null"]},
                    "kind": {"type": ["string", "null"]}
                }
            })
        );
    }

    struct StubTool;

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn description(&self) -> &'static str {
            "test stub"
        }
        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
            Ok(json!({"ok": true}))
        }
    }

    #[tokio::test]
    async fn tool_dispatches() {
        let t = StubTool;
        let ctx = ToolCtx::new(Subject::new("u"));
        let r = t.execute(json!({}), &ctx).await.expect("ok");
        assert_eq!(r["ok"], true);
    }

    #[tokio::test]
    async fn tool_execute_result_defaults_to_success() {
        let t = StubTool;
        let ctx = ToolCtx::new(Subject::new("u"));
        let r = t.execute_result(json!({}), &ctx).await.expect("ok");
        assert_eq!(r.output, json!({"ok": true}));
        assert!(!r.is_error);
    }

    struct SoftErrorTool;

    #[async_trait]
    impl Tool for SoftErrorTool {
        fn name(&self) -> &'static str {
            "soft_error"
        }
        fn description(&self) -> &'static str {
            "soft error stub"
        }
        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
            Err(ToolError::Execution(
                "soft error test tool uses execute_result".to_owned(),
            ))
        }
        async fn execute_result(
            &self,
            _args: Value,
            _ctx: &ToolCtx,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::soft_error(json!("validation failed")))
        }
    }

    #[tokio::test]
    async fn tool_execute_result_can_signal_soft_error() {
        let t = SoftErrorTool;
        let ctx = ToolCtx::new(Subject::new("u"));
        let r = t.execute_result(json!({}), &ctx).await.expect("ok");
        assert_eq!(r.output, json!("validation failed"));
        assert!(r.is_error);
    }

    #[derive(Clone, Copy)]
    enum TestOp {
        List,
        Get,
    }

    impl ToolOp for TestOp {
        const JSON_VALUES: &'static [&'static str] = &["list", "get"];

        fn as_str(self) -> &'static str {
            match self {
                Self::List => "list",
                Self::Get => "get",
            }
        }
    }

    #[test]
    fn op_schema_uses_declared_json_values() {
        assert_eq!(TestOp::List.as_str(), "list");
        assert_eq!(TestOp::Get.as_str(), "get");
        assert_eq!(
            op_schema::<TestOp>("operation"),
            json!({
                "type": "string",
                "enum": ["list", "get"],
                "description": "operation",
            })
        );
    }

    #[test]
    fn soft_error_object_uses_standard_shape() {
        let result = soft_error_object("try again");

        assert!(result.is_error);
        assert_eq!(result.output, json!({ "error": "try again" }));
    }

    #[test]
    fn tool_ctx_default_no_cancel() {
        let ctx = ToolCtx::new(Subject::new("u"));
        assert!(ctx.cancel.is_none());
        assert!(!ctx.is_cancelled());
    }

    #[test]
    fn tool_ctx_reports_cancellation() {
        let token = CancellationToken::new();
        let ctx = ToolCtx::new(Subject::new("u")).with_cancel(token.clone());
        assert!(!ctx.is_cancelled());
        token.cancel();
        assert!(ctx.is_cancelled());
    }

    #[derive(Debug, Deserialize)]
    struct RequiredArg {
        name: String,
    }

    #[test]
    fn parse_args_maps_serde_errors_to_invalid_args() {
        let err = parse_args::<RequiredArg>(json!({})).expect_err("missing required field");

        assert!(matches!(err, ToolError::InvalidArgs(message) if message.contains("name")));
    }

    #[test]
    fn parse_args_returns_typed_args() {
        let args = parse_args::<RequiredArg>(json!({"name": "read_file"})).expect("valid args");

        assert_eq!(args.name, "read_file");
    }

    #[test]
    fn parse_args_with_context_prefixes_serde_errors() {
        let err = parse_args_with_context::<RequiredArg>(json!({}), "memory args")
            .expect_err("missing required field");

        assert!(matches!(err, ToolError::InvalidArgs(message) if message.contains("memory args")));
    }

    #[test]
    fn clamp_positive_limit_rejects_zero_and_clamps_to_max() {
        let err = clamp_positive_limit(0, 100, "memory.search").expect_err("zero limit");

        assert!(
            matches!(err, ToolError::InvalidArgs(message) if message.contains("memory.search"))
        );
        assert_eq!(
            clamp_positive_limit(120, 100, "memory.search").expect("test result"),
            100
        );
    }

    #[test]
    fn clamp_optional_usize_limit_defaults_and_clamps() {
        assert_eq!(clamp_optional_usize_limit(None, 32, 100), 32);
        assert_eq!(clamp_optional_usize_limit(Some(120), 32, 100), 100);
        assert_eq!(clamp_optional_usize_limit(Some(8), 32, 100), 8);
    }
}
