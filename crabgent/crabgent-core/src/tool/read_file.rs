//! `read_file` builtin: read a file from disk and return its contents.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs::File;
use tokio::io::AsyncReadExt;

use crate::error::ToolError;
use crate::tool::{Tool, ToolCtx, parse_args};
use crate::types::ToolResult;

use super::file_common::soft_file_error_or;
use super::path_root::{existing_path_io_error, validate_existing_path};

const DEFAULT_MAX_BYTES: u64 = 30 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Args {
    path: PathBuf,
    /// Optional byte cap. Defaults to the tool's configured cap (30 MiB).
    max_bytes: Option<u64>,
}

/// Read a file from disk and return its UTF-8 contents.
///
/// Construct with [`ReadFileTool::new`] to confine every access under a root
/// directory (the safe default). [`ReadFileTool::without_root`] is the explicit
/// Wild-West escape hatch with no path confinement.
pub struct ReadFileTool {
    max_bytes: u64,
    root: Option<PathBuf>,
}

impl ReadFileTool {
    /// Confine reads under `root`: relative paths resolve inside it and any
    /// path escaping it (via `..` or an absolute path) is rejected.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            root: Some(root.into()),
        }
    }

    /// Build a tool with NO path confinement: the LLM-supplied path opens any
    /// readable file on the host, including `/etc/passwd` or SSH keys. Only use
    /// this in a fully trusted Wild-West setup. Prefer [`ReadFileTool::new`].
    #[must_use]
    pub const fn without_root() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            root: None,
        }
    }

    /// Override the byte cap, keeping the configured root confinement.
    #[must_use]
    pub const fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read the contents of a file from disk and return as UTF-8 text. Truncates to a per-tool byte cap (default 30 MiB). With a configured root the path must stay under it; an unconfined tool reads any path the host process can access."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "absolute or relative path"},
                "max_bytes": {"type": "integer", "description": "optional byte cap"}
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: Args = parse_args(args)?;
        let cap_bytes = args.max_bytes.unwrap_or(self.max_bytes);
        let cap_usize = usize::try_from(cap_bytes).unwrap_or(usize::MAX);
        let path = validate_existing_path(&args.path, self.root.as_deref())?;
        let file = File::open(&path)
            .await
            .map_err(|err| existing_path_io_error(&path, &err))?;
        let total = file
            .metadata()
            .await
            .map_err(|e| ToolError::Io(e.to_string()))?
            .len();
        let read_limit = cap_bytes.saturating_add(1);
        let mut bytes = Vec::with_capacity(cap_usize.saturating_add(1));
        let mut limited = file.take(read_limit);
        limited
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| ToolError::Io(e.to_string()))?;
        let truncated = bytes.len() > cap_usize;
        let mut content = super::safe_truncate(&bytes, cap_usize);
        if truncated {
            content.push_str(super::TRUNCATE_MARKER);
        }
        Ok(json!({
            "content": content,
            "size_bytes": total,
            "truncated": truncated
        }))
    }

    /// Map `NotFound` and `Io` to soft errors via
    /// [`soft_file_error_or`]; a missing path is the canonical
    /// LLM-recoverable case and stream-level `Io` failures (mid-read disk
    /// error, metadata stat) get the same treatment. Hard variants stay
    /// authoritative.
    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        soft_file_error_or(self.execute(args, ctx).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subject::Subject;
    use tempfile::tempdir;

    fn ctx() -> ToolCtx {
        ToolCtx::new(Subject::new("u"))
    }

    #[tokio::test]
    async fn reads_existing_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello").expect("write");
        let tool = ReadFileTool::without_root();
        let r = tool
            .execute(
                json!({"path": path.to_str().expect("path should be valid UTF-8")}),
                &ctx(),
            )
            .await
            .expect("ok");
        assert_eq!(r["content"], "hello");
        assert_eq!(r["size_bytes"], 5);
        assert_eq!(r["truncated"], false);
    }

    #[tokio::test]
    async fn missing_file_returns_not_found() {
        let tool = ReadFileTool::without_root();
        let r = tool
            .execute(json!({"path": "/no/such/path/here.txt"}), &ctx())
            .await;
        assert!(matches!(r, Err(ToolError::NotFound(_))));
    }

    #[tokio::test]
    async fn invalid_args_errors() {
        let tool = ReadFileTool::without_root();
        let r = tool.execute(json!({"oops": 1}), &ctx()).await;
        assert!(matches!(r, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn truncates_when_over_cap() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("big.txt");
        std::fs::write(&path, "abcdefghij").expect("write");
        let tool = ReadFileTool::without_root().with_max_bytes(4);
        let r = tool
            .execute(
                json!({"path": path.to_str().expect("path should be valid UTF-8")}),
                &ctx(),
            )
            .await
            .expect("ok");
        assert_eq!(r["truncated"], true);
        assert_eq!(r["size_bytes"], 10);
        let s = r["content"].as_str().expect("string");
        assert!(s.starts_with("abcd"), "got: {s}");
        assert!(s.contains("truncated"), "got: {s}");
    }

    #[tokio::test]
    async fn per_call_max_bytes_overrides() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("big.txt");
        std::fs::write(&path, "0123456789").expect("write");
        let tool = ReadFileTool::without_root();
        let r = tool
            .execute(
                json!({"path": path.to_str().expect("path should be valid UTF-8"), "max_bytes": 3}),
                &ctx(),
            )
            .await
            .expect("ok");
        assert_eq!(r["truncated"], true);
        let s = r["content"].as_str().expect("string");
        assert!(s.starts_with("012"));
    }

    #[tokio::test]
    async fn root_allows_file_inside_root() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir(&root).expect("root");
        let path = root.join("allowed.txt");
        std::fs::write(&path, "inside").expect("write");
        let tool = ReadFileTool::new(&root);
        let r = tool
            .execute(json!({"path": "allowed.txt"}), &ctx())
            .await
            .expect("ok");
        assert_eq!(r["content"], "inside");
    }

    #[tokio::test]
    async fn root_rejects_traversal_path() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&root).expect("root");
        std::fs::create_dir(&outside).expect("outside");
        std::fs::write(outside.join("secret.txt"), "secret").expect("write");
        let tool = ReadFileTool::new(&root);
        let r = tool
            .execute(json!({"path": "../outside/secret.txt"}), &ctx())
            .await;
        assert!(matches!(r, Err(ToolError::Permission(_))));
    }

    #[tokio::test]
    async fn root_rejects_absolute_path_outside_root() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        let outside = dir.path().join("outside.txt");
        std::fs::create_dir(&root).expect("root");
        std::fs::write(&outside, "secret").expect("write");
        let tool = ReadFileTool::new(&root);
        let r = tool
            .execute(
                json!({"path": outside.to_str().expect("path should be valid UTF-8")}),
                &ctx(),
            )
            .await;
        assert!(matches!(r, Err(ToolError::Permission(_))));
    }

    #[tokio::test]
    async fn confined_rejects_absolute_system_path() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir(&root).expect("root");
        let tool = ReadFileTool::new(&root);
        let r = tool.execute(json!({"path": "/etc/passwd"}), &ctx()).await;
        assert!(matches!(r, Err(ToolError::Permission(_))));
    }

    #[tokio::test]
    async fn unconfined_keeps_absolute_path_behavior() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("open.txt");
        std::fs::write(&path, "open").expect("write");
        let tool = ReadFileTool::without_root();
        let r = tool
            .execute(
                json!({"path": path.to_str().expect("path should be valid UTF-8")}),
                &ctx(),
            )
            .await
            .expect("ok");
        assert_eq!(r["content"], "open");
    }

    #[test]
    fn schema_declares_path_required() {
        let tool = ReadFileTool::without_root();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().expect("required");
        assert!(required.iter().any(|v| v == "path"));
    }

    #[tokio::test]
    async fn truncate_respects_utf8_boundary_at_cap() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("utf8.txt");
        std::fs::write(&path, "abcäxyz").expect("write");
        let tool = ReadFileTool::without_root().with_max_bytes(4);
        let r = tool
            .execute(
                json!({"path": path.to_str().expect("path should be valid UTF-8")}),
                &ctx(),
            )
            .await
            .expect("ok");

        let content = r["content"].as_str().expect("content");
        assert_eq!(content, "abc\n... [truncated]");
        assert!(!content.contains('\u{FFFD}'));
    }
}
