//! `write_file` builtin: create or overwrite a file with given content.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs;

use crate::error::ToolError;
use crate::tool::{Tool, ToolCtx, parse_args};

use super::path_root::validate_creatable_path;

#[derive(Debug, Deserialize)]
struct Args {
    path: PathBuf,
    content: String,
    /// Whether to create missing parent directories. Defaults to `true`
    /// to match common agent-tool ergonomics.
    create_parents: Option<bool>,
}

/// Create or overwrite a file with the given content.
///
/// Construct with [`WriteFileTool::new`] to confine every write under a root
/// directory (the safe default). [`WriteFileTool::without_root`] is the explicit
/// Wild-West escape hatch with no path confinement.
pub struct WriteFileTool {
    root: Option<PathBuf>,
}

impl WriteFileTool {
    /// Confine writes under `root`: relative paths resolve inside it and any
    /// path escaping it (via `..`, an absolute path, or a symlink) is rejected.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Some(root.into()),
        }
    }

    /// Build a tool with NO path confinement: the LLM-supplied path writes
    /// anywhere the host process can, including overwriting dotfiles or system
    /// files. Only use this in a fully trusted Wild-West setup. Prefer
    /// [`WriteFileTool::new`].
    #[must_use]
    pub const fn without_root() -> Self {
        Self { root: None }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "Create or overwrite a file with the given content. Creates parent directories by default; pass create_parents=false to disable. With a configured root the path must stay under it; an unconfined tool writes any path the host process can access."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"},
                "create_parents": {"type": "boolean", "default": true}
            },
            "required": ["path", "content"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: Args = parse_args(args)?;
        let create_parents = args.create_parents.unwrap_or(true);
        let path = validate_creatable_path(&args.path, self.root.as_deref())?;
        if create_parents
            && let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| ToolError::Io(e.to_string()))?;
        }
        fs::write(&path, &args.content)
            .await
            .map_err(|e| ToolError::Io(e.to_string()))?;
        Ok(json!({
            "path": path.display().to_string(),
            "size_bytes": args.content.len()
        }))
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
    async fn writes_new_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("new.txt");
        let tool = WriteFileTool::without_root();
        let r = tool
            .execute(
                json!({"path": path.to_str().expect("path should be valid UTF-8"), "content": "hello"}),
                &ctx(),
            )
            .await
            .expect("ok");
        assert_eq!(r["size_bytes"], 5);
        let body = std::fs::read_to_string(&path).expect("read");
        assert_eq!(body, "hello");
    }

    #[tokio::test]
    async fn overwrites_existing_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "old content").expect("write");
        let tool = WriteFileTool::without_root();
        tool.execute(
            json!({"path": path.to_str().expect("path should be valid UTF-8"), "content": "new"}),
            &ctx(),
        )
        .await
        .expect("ok");
        let body = std::fs::read_to_string(&path).expect("read");
        assert_eq!(body, "new");
    }

    #[tokio::test]
    async fn creates_parent_directories_by_default() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("a/b/c/file.txt");
        let tool = WriteFileTool::without_root();
        tool.execute(
            json!({"path": path.to_str().expect("path should be valid UTF-8"), "content": "x"}),
            &ctx(),
        )
        .await
        .expect("ok");
        assert!(path.exists());
    }

    #[tokio::test]
    async fn create_parents_false_fails_on_missing_dir() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("missing/file.txt");
        let tool = WriteFileTool::without_root();
        let r = tool
            .execute(
                json!({
                    "path": path.to_str().expect("path should be valid UTF-8"),
                    "content": "x",
                    "create_parents": false
                }),
                &ctx(),
            )
            .await;
        assert!(matches!(r, Err(ToolError::Io(_))));
    }

    #[tokio::test]
    async fn invalid_args_errors() {
        let tool = WriteFileTool::without_root();
        let r = tool.execute(json!({"path": "x"}), &ctx()).await;
        assert!(matches!(r, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn root_allows_file_inside_root() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir(&root).expect("root");
        let tool = WriteFileTool::new(&root);
        let r = tool
            .execute(
                json!({"path": "nested/allowed.txt", "content": "inside"}),
                &ctx(),
            )
            .await
            .expect("ok");
        assert_eq!(r["size_bytes"], 6);
        assert_eq!(
            std::fs::read_to_string(root.join("nested/allowed.txt")).expect("read"),
            "inside"
        );
    }

    #[tokio::test]
    async fn root_rejects_traversal_path() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir(&root).expect("root");
        let tool = WriteFileTool::new(&root);
        let r = tool
            .execute(
                json!({"path": "../outside.txt", "content": "secret"}),
                &ctx(),
            )
            .await;
        assert!(matches!(r, Err(ToolError::Permission(_))));
        assert!(!dir.path().join("outside.txt").exists());
    }

    #[tokio::test]
    async fn root_rejects_absolute_path_outside_root() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        let outside = dir.path().join("outside.txt");
        std::fs::create_dir(&root).expect("root");
        let tool = WriteFileTool::new(&root);
        let r = tool
            .execute(
                json!({"path": outside.to_str().expect("path should be valid UTF-8"), "content": "secret"}),
                &ctx(),
            )
            .await;
        assert!(matches!(
            r,
            Err(ToolError::Permission(reason))
                if !reason.contains(outside.to_str().expect("path should be valid UTF-8"))
                    && !reason.contains(root.to_str().expect("path should be valid UTF-8"))
        ));
        assert!(!outside.exists());
    }

    #[tokio::test]
    async fn confined_rejects_absolute_system_path() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir(&root).expect("root");
        let tool = WriteFileTool::new(&root);
        let r = tool
            .execute(
                json!({"path": "/etc/crabgent-should-not-exist.txt", "content": "x"}),
                &ctx(),
            )
            .await;
        assert!(matches!(r, Err(ToolError::Permission(_))));
        assert!(!std::path::Path::new("/etc/crabgent-should-not-exist.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn root_rejects_symlink_to_outside_root() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().expect("tempdir");
        let root = dir.path().join("root");
        let outside = dir.path().join("outside.txt");
        let link = root.join("link.txt");
        std::fs::create_dir(&root).expect("root");
        std::fs::write(&outside, "outside").expect("write");
        symlink(&outside, &link).expect("symlink");

        let tool = WriteFileTool::new(&root);
        let r = tool
            .execute(json!({"path": "link.txt", "content": "secret"}), &ctx())
            .await;

        assert!(matches!(
            r,
            Err(ToolError::Permission(reason))
                if !reason.contains(outside.to_str().expect("path should be valid UTF-8"))
                    && !reason.contains(root.to_str().expect("path should be valid UTF-8"))
        ));
        assert_eq!(std::fs::read_to_string(outside).expect("read"), "outside");
    }
}
