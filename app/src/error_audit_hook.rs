//! Tool-error audit sink: appends every `is_error` tool result to a
//! local JSONL file for the offline error-review loop.
//!
//! Why a side file and not the session log: the kernel compacts
//! sessions, so tool errors vanish into summaries within a few turns.
//! The review loop (`scripts/agent-error-review.py`) needs a durable,
//! groupable record of `agent + tool + error` over days. This hook is
//! the capture-at-occurrence half; the script is the
//! aggregate-and-learn half that writes per-agent `tools`-class shared
//! memory the agent re-reads next turn via the recall hook's pinned
//! block (see `memory_recall_hook::SHARED_PINNED_CLASSES`).
//!
//! Observe-only: `after_tool` never transforms the result
//! (`Decision::Continue`) and never fails the run. A write error is
//! logged and swallowed. The hook is built per agent kernel, so the
//! `agent` stamp is the kernel's own name (no reverse owner mapping).

use std::path::PathBuf;

use async_trait::async_trait;
use crabgent_core::{Decision, Hook, RunCtx, ToolCall, ToolResult};
use crabgent_log::warn;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

/// Cap on the stored error string. Keeps each JSONL line small enough
/// that the single `O_APPEND` write stays atomic across concurrent runs
/// and agents sharing one file.
const ERROR_CAP: usize = 800;

pub struct ErrorAuditHook {
    path: PathBuf,
    agent: String,
}

impl ErrorAuditHook {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, agent: &str) -> Self {
        Self {
            path: path.into(),
            agent: agent.to_owned(),
        }
    }

    async fn record(&self, tool: &str, error: &str, owner: &str) {
        let line = json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "agent": self.agent,
            "tool": tool,
            "owner": owner,
            "error": first_line_capped(error, ERROR_CAP),
        });
        let mut buf = match serde_json::to_string(&line) {
            Ok(s) => s,
            Err(err) => {
                warn!(error = %err, "error-audit: serialize failed");
                return;
            }
        };
        buf.push('\n');
        // O_APPEND: each write lands at EOF atomically, so concurrent
        // runs and agents sharing one file never overwrite each other.
        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
        {
            Ok(mut file) => {
                if let Err(err) = file.write_all(buf.as_bytes()).await {
                    warn!(error = %err, "error-audit: append failed");
                }
            }
            Err(err) => {
                warn!(error = %err, path = %self.path.display(), "error-audit: open failed");
            }
        }
    }
}

#[async_trait]
impl Hook for ErrorAuditHook {
    async fn after_tool(
        &self,
        call: &ToolCall,
        result: &ToolResult,
        ctx: &RunCtx,
    ) -> Decision<ToolResult> {
        if result.is_error {
            let error = error_text(&result.output);
            self.record(&call.name, &error, ctx.subject.id()).await;
        }
        Decision::Continue
    }
}

/// Render a tool-result output into a short error string. JSON strings
/// pass through unquoted; any other shape is stringified.
fn error_text(output: &Value) -> String {
    output
        .as_str()
        .map_or_else(|| output.to_string(), str::to_owned)
}

/// First non-empty trimmed line, truncated to `max` bytes on a char
/// boundary.
fn first_line_capped(s: &str, max: usize) -> String {
    let head = s
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if head.len() <= max {
        return head.to_owned();
    }
    let mut end = max;
    while end > 0 && !head.is_char_boundary(end) {
        end -= 1;
    }
    head[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::{RunId, Subject};

    #[test]
    fn first_line_capped_keeps_short_single_line() {
        assert_eq!(first_line_capped("boom", 800), "boom");
    }

    #[test]
    fn first_line_capped_takes_first_nonempty_line() {
        assert_eq!(
            first_line_capped("\n  \nreal error\nmore", 800),
            "real error"
        );
    }

    #[test]
    fn first_line_capped_truncates_on_char_boundary() {
        let s = "äöü".repeat(10); // 60 bytes, 30 chars
        let out = first_line_capped(&s, 5);
        assert!(out.len() <= 5);
        assert!(s.starts_with(&out));
    }

    #[test]
    fn first_line_capped_empty_input() {
        assert_eq!(first_line_capped("   \n  ", 800), "");
    }

    #[test]
    fn error_text_passes_string_through() {
        assert_eq!(error_text(&json!("plain error")), "plain error");
    }

    #[test]
    fn error_text_stringifies_object() {
        assert_eq!(error_text(&json!({"code": 42})), "{\"code\":42}");
    }

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("crabgent-audit-{}.jsonl", uuid::Uuid::new_v4()))
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: "c1".to_owned(),
            name: name.to_owned(),
            args: json!({}),
            thought_signature: None,
        }
    }

    fn ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("telegram:42"))
    }

    #[tokio::test]
    async fn after_tool_appends_one_line_for_error() {
        let path = temp_path();
        let hook = ErrorAuditHook::new(path.clone(), "local");
        let result = ToolResult::soft_error(json!("kube context not in cluster registry"));
        let decision = hook.after_tool(&call("kube"), &result, &ctx()).await;
        assert!(matches!(decision, Decision::Continue));

        let body = tokio::fs::read_to_string(&path)
            .await
            .expect("file written");
        let _ = tokio::fs::remove_file(&path).await;
        let parsed: Value = serde_json::from_str(body.trim()).expect("one json line");
        assert_eq!(parsed["agent"], json!("local"));
        assert_eq!(parsed["tool"], json!("kube"));
        assert_eq!(parsed["owner"], json!("telegram:42"));
        assert_eq!(
            parsed["error"],
            json!("kube context not in cluster registry")
        );
        assert!(parsed["ts"].as_str().is_some());
    }

    #[tokio::test]
    async fn after_tool_ignores_success() {
        let path = temp_path();
        let hook = ErrorAuditHook::new(path.clone(), "local");
        let result = ToolResult::success(json!("ok"));
        hook.after_tool(&call("bash"), &result, &ctx()).await;
        assert!(
            tokio::fs::metadata(&path).await.is_err(),
            "no file should be created for a successful tool call"
        );
    }

    #[tokio::test]
    async fn after_tool_appends_multiple_errors() {
        let path = temp_path();
        let hook = ErrorAuditHook::new(path.clone(), "assistant");
        for _ in 0..3 {
            let result = ToolResult::soft_error(json!("404 not found"));
            hook.after_tool(&call("github_read"), &result, &ctx()).await;
        }
        let body = tokio::fs::read_to_string(&path)
            .await
            .expect("file written");
        let _ = tokio::fs::remove_file(&path).await;
        let line_count = body.lines().filter(|l| !l.is_empty()).count();
        assert_eq!(line_count, 3);
    }
}
