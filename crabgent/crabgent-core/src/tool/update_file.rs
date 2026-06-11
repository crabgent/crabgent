//! `update_file` builtin: anchor-replace edit. Find `old_str`, replace
//! with `new_str`. Fails if not found or ambiguous (unless `replace_all`).

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs;

use crate::error::ToolError;
use crate::tool::{Tool, ToolCtx, parse_args};
use crate::types::ToolResult;

use super::file_common::soft_file_error_or;
use super::path_root::{existing_path_io_error, validate_existing_path};

#[derive(Debug, Deserialize)]
struct Args {
    path: PathBuf,
    old_str: String,
    new_str: String,
    replace_all: Option<bool>,
}

/// Anchor-replace edit: find `old_str` in the file, replace with `new_str`.
///
/// Fails with `NotFound` if the anchor does not occur, and with
/// `InvalidArgs` if it occurs more than once and `replace_all` is not
/// set. With `replace_all = true`, all occurrences are replaced.
///
/// Construct with [`UpdateFileTool::new`] to confine every edit under a root
/// directory (the safe default). [`UpdateFileTool::without_root`] is the
/// explicit Wild-West escape hatch with no path confinement.
pub struct UpdateFileTool {
    root: Option<PathBuf>,
}

impl UpdateFileTool {
    /// Confine edits under `root`: relative paths resolve inside it and any
    /// path escaping it (via `..` or an absolute path) is rejected.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Some(root.into()),
        }
    }

    /// Build a tool with NO path confinement: the LLM-supplied path edits any
    /// writable file on the host, including dotfiles or system files. Only use
    /// this in a fully trusted Wild-West setup. Prefer [`UpdateFileTool::new`].
    #[must_use]
    pub const fn without_root() -> Self {
        Self { root: None }
    }
}

#[async_trait]
impl Tool for UpdateFileTool {
    fn name(&self) -> &'static str {
        "update_file"
    }
    fn description(&self) -> &'static str {
        "Replace `old_str` with `new_str` in a file. Fails if `old_str` is not found, or if it occurs more than once unless `replace_all` is true. With a configured root the path must stay under it; an unconfined tool edits any path the host process can access."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old_str": {"type": "string"},
                "new_str": {"type": "string"},
                "replace_all": {"type": "boolean", "default": false}
            },
            "required": ["path", "old_str", "new_str"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: Args = parse_args(args)?;
        let path = validate_existing_path(&args.path, self.root.as_deref())?;
        let bytes = fs::read(&path)
            .await
            .map_err(|err| existing_path_io_error(&path, &err))?;
        let original = String::from_utf8(bytes)
            .map_err(|e| ToolError::Io(format!("file is not valid UTF-8: {e}")))?;
        let count = original.matches(&args.old_str).count();
        if count == 0 {
            return Err(ToolError::NotFound(format!(
                "anchor not found in {}",
                path.display()
            )));
        }
        let replace_all = args.replace_all.unwrap_or(false);
        if count > 1 && !replace_all {
            return Err(ToolError::InvalidArgs(format!(
                "anchor occurs {count} times; pass replace_all=true to replace all, or supply more context to make the match unique"
            )));
        }
        let updated = if replace_all {
            original.replace(&args.old_str, &args.new_str)
        } else {
            original.replacen(&args.old_str, &args.new_str, 1)
        };
        fs::write(&path, &updated)
            .await
            .map_err(|e| ToolError::Io(e.to_string()))?;
        Ok(json!({
            "path": path.display().to_string(),
            "replacements": if replace_all { count } else { 1 },
            "size_bytes": updated.len()
        }))
    }

    /// Map `NotFound` (missing file or missing anchor) and `Io`
    /// (write/read failure, non-UTF-8 content) to soft errors via
    /// [`soft_file_error_or`] so the LLM can repair the call: pick a
    /// different path, widen the anchor, or stop. `InvalidArgs` (ambiguous
    /// anchor) and the other hard variants stay authoritative.
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
    async fn replaces_unique_anchor() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "before X after").expect("write");
        let tool = UpdateFileTool::without_root();
        let r = tool
            .execute(
                json!({
                    "path": path.to_str().expect("path should be valid UTF-8"),
                    "old_str": "X",
                    "new_str": "Y"
                }),
                &ctx(),
            )
            .await
            .expect("ok");
        assert_eq!(r["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(&path).expect("test result"),
            "before Y after"
        );
    }

    #[tokio::test]
    async fn missing_anchor_returns_not_found() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "no match here").expect("write");
        let tool = UpdateFileTool::without_root();
        let r = tool
            .execute(
                json!({
                    "path": path.to_str().expect("path should be valid UTF-8"),
                    "old_str": "Q",
                    "new_str": "R"
                }),
                &ctx(),
            )
            .await;
        assert!(matches!(r, Err(ToolError::NotFound(_))));
    }

    #[tokio::test]
    async fn ambiguous_anchor_without_replace_all() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "X X X").expect("write");
        let tool = UpdateFileTool::without_root();
        let r = tool
            .execute(
                json!({
                    "path": path.to_str().expect("path should be valid UTF-8"),
                    "old_str": "X",
                    "new_str": "Y"
                }),
                &ctx(),
            )
            .await;
        assert!(matches!(r, Err(ToolError::InvalidArgs(_))));
        assert_eq!(
            std::fs::read_to_string(&path).expect("test result"),
            "X X X"
        );
    }

    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "X X X").expect("write");
        let tool = UpdateFileTool::without_root();
        let r = tool
            .execute(
                json!({
                    "path": path.to_str().expect("path should be valid UTF-8"),
                    "old_str": "X",
                    "new_str": "Y",
                    "replace_all": true
                }),
                &ctx(),
            )
            .await
            .expect("ok");
        assert_eq!(r["replacements"], 3);
        assert_eq!(
            std::fs::read_to_string(&path).expect("test result"),
            "Y Y Y"
        );
    }

    #[tokio::test]
    async fn missing_file_returns_not_found() {
        let tool = UpdateFileTool::without_root();
        let r = tool
            .execute(
                json!({
                    "path": "/no/such/file/here.txt",
                    "old_str": "X",
                    "new_str": "Y"
                }),
                &ctx(),
            )
            .await;
        assert!(matches!(r, Err(ToolError::NotFound(_))));
    }

    #[tokio::test]
    async fn invalid_args_errors() {
        let tool = UpdateFileTool::without_root();
        let r = tool.execute(json!({"path": "x"}), &ctx()).await;
        assert!(matches!(r, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn root_allows_file_inside_root() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir(&root).expect("root");
        let path = root.join("allowed.txt");
        std::fs::write(&path, "before X after").expect("write");
        let tool = UpdateFileTool::new(&root);
        tool.execute(
            json!({"path": "allowed.txt", "old_str": "X", "new_str": "Y"}),
            &ctx(),
        )
        .await
        .expect("ok");
        assert_eq!(
            std::fs::read_to_string(path).expect("read"),
            "before Y after"
        );
    }

    #[tokio::test]
    async fn root_rejects_traversal_path() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&root).expect("root");
        std::fs::create_dir(&outside).expect("outside");
        std::fs::write(outside.join("secret.txt"), "X").expect("write");
        let tool = UpdateFileTool::new(&root);
        let r = tool
            .execute(
                json!({"path": "../outside/secret.txt", "old_str": "X", "new_str": "Y"}),
                &ctx(),
            )
            .await;
        assert!(matches!(r, Err(ToolError::Permission(_))));
        assert_eq!(
            std::fs::read_to_string(outside.join("secret.txt")).expect("read"),
            "X"
        );
    }

    #[tokio::test]
    async fn root_rejects_absolute_path_outside_root() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        let outside = dir.path().join("outside.txt");
        std::fs::create_dir(&root).expect("root");
        std::fs::write(&outside, "X").expect("write");
        let tool = UpdateFileTool::new(&root);
        let r = tool
            .execute(
                json!({"path": outside.to_str().expect("path should be valid UTF-8"), "old_str": "X", "new_str": "Y"}),
                &ctx(),
            )
            .await;
        assert!(matches!(r, Err(ToolError::Permission(_))));
        assert_eq!(std::fs::read_to_string(outside).expect("read"), "X");
    }

    #[tokio::test]
    async fn confined_rejects_absolute_system_path() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir(&root).expect("root");
        let tool = UpdateFileTool::new(&root);
        let r = tool
            .execute(
                json!({"path": "/etc/passwd", "old_str": "root", "new_str": "x"}),
                &ctx(),
            )
            .await;
        assert!(matches!(r, Err(ToolError::Permission(_))));
    }
}
