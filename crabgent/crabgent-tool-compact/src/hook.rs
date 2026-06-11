//! [`ToolCompactHook`]: the fail-closed `after_tool` compaction hook.
//!
//! On `after_tool` the hook does only the cheap pre-checks (skip the recall
//! tool's own output, honor the per-run auto-disable) and then delegates the
//! whole content analysis to [`Compactor::run`]. It maps the verdict to a
//! `Decision`, owning the stash into the [`ToolCacheStore`] and the coverage
//! footer. Every failure path forwards the raw output unchanged (fail-open),
//! so the per-tool byte caps remain the floor.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use crabgent_core::hook::{Decision, Hook, Outcome, RunCtx};
use crabgent_core::text::floor_char_boundary;
use crabgent_core::types::{ToolCall, ToolResult};
use crabgent_log::warn;
use crabgent_store::{ToolCacheEntry, ToolCacheStore};
use serde_json::Value;

use crate::autodisable::AutoDisableTracker;
use crate::compactor::{Compactor, CompactorVerdict, UNCERTAIN_MARKER};
use crate::config::ToolCompactConfig;
use crate::filters::CompactInput;
use crate::footer::render_footer;
use crate::handle::RecallHandle;
use crate::recall::RECALL_TOOL_NAME;
use crate::session::{SessionResolver, default_session_resolver};
use crate::stats::CompactionStats;

/// Bytes of the compacted body kept as the stash entry's preview.
const PREVIEW_BYTES: usize = 256;

/// Hook that compacts oversized tool outputs and stashes the full original.
pub struct ToolCompactHook<C: ToolCacheStore> {
    store: Arc<C>,
    resolve_session: SessionResolver,
    tracker: AutoDisableTracker,
    compactor: Compactor,
    ttl: Duration,
    autodisable_n: u32,
}

impl<C: ToolCacheStore> ToolCompactHook<C> {
    /// Build the hook from a store and config. Shares the auto-disable tracker
    /// and session resolver with the recall tool via the builder.
    pub fn new(store: Arc<C>, config: ToolCompactConfig) -> Self {
        let ttl = config.ttl;
        let autodisable_n = config.autodisable_n;
        Self {
            store,
            resolve_session: default_session_resolver(),
            tracker: AutoDisableTracker::new(),
            compactor: Compactor::new(config),
            ttl,
            autodisable_n,
        }
    }

    /// Share the auto-disable tracker with the recall tool.
    #[must_use]
    pub fn with_tracker(mut self, tracker: AutoDisableTracker) -> Self {
        self.tracker = tracker;
        self
    }

    /// Share the session resolver with the recall tool.
    #[must_use]
    pub fn with_shared_session_resolver(mut self, resolver: SessionResolver) -> Self {
        self.resolve_session = resolver;
        self
    }

    async fn stash_and_replace(
        &self,
        call: &ToolCall,
        result: &ToolResult,
        ctx: &RunCtx,
        content: &str,
        body: String,
        stats: &CompactionStats,
    ) -> Decision<ToolResult> {
        let handle = RecallHandle::new(&ctx.run_id, content);
        let now = Utc::now();
        let preview_end = floor_char_boundary(body.as_str(), PREVIEW_BYTES.min(body.len()));
        let preview = body.get(..preview_end).unwrap_or_default().to_owned();
        let entry = ToolCacheEntry {
            id: handle.to_string(),
            session_id: (self.resolve_session)(&ctx.subject),
            tool_name: call.name.clone(),
            content: content.to_owned(),
            preview,
            created_at: now,
            expires_at: now + self.ttl,
        };
        if let Err(err) = self.store.insert(&entry).await {
            warn!(
                tool = %call.name,
                error_kind = err.kind(),
                transient = err.is_transient(),
                "tool-compact: stash insert failed; leaving inline output"
            );
            return Decision::Continue;
        }
        let footer = render_footer(stats, &handle);
        Decision::Replace(replace_output(result, format!("{body}\n{footer}")))
    }
}

#[async_trait]
impl<C> Hook for ToolCompactHook<C>
where
    C: ToolCacheStore + 'static,
{
    async fn after_tool(
        &self,
        call: &ToolCall,
        result: &ToolResult,
        ctx: &RunCtx,
    ) -> Decision<ToolResult> {
        if call.name == RECALL_TOOL_NAME {
            return Decision::Continue;
        }
        if self
            .tracker
            .is_disabled(&ctx.run_id, &call.name, self.autodisable_n)
            .await
        {
            return Decision::Continue;
        }
        let Some(content) = extract_content(call, &result.output) else {
            return Decision::Continue;
        };
        let input = CompactInput {
            content: &content,
            tool_name: &call.name,
            bash_command: bash_command(call),
            exit_code: bash_exit_code(call, &result.output),
            is_error: result.is_error,
        };
        match self.compactor.run(&input) {
            CompactorVerdict::Passthrough => Decision::Continue,
            CompactorVerdict::UncertainMarker => {
                // Prefer the human-readable rendering (bash stdout/stderr with
                // real newlines) over escaped JSON; fall back to the verbatim
                // stringify only when there is no extractable content.
                let rendered = extract_content(call, &result.output)
                    .unwrap_or_else(|| stringify_output(&result.output));
                let text = format!("{UNCERTAIN_MARKER}\n{rendered}");
                Decision::Replace(replace_output(result, text))
            }
            CompactorVerdict::Compacted { body, stats } => {
                self.stash_and_replace(call, result, ctx, &content, body, &stats)
                    .await
            }
        }
    }

    async fn on_stop(&self, ctx: &RunCtx, _outcome: &Outcome) {
        self.tracker.clear_run(&ctx.run_id).await;
    }
}

/// Build a replacement result, preserving `call_id` and `is_error`.
fn replace_output(result: &ToolResult, text: String) -> ToolResult {
    let mut replaced = result.clone();
    replaced.output = Value::String(text);
    replaced
}

/// The text the compactor analyzes. For `bash` this is stdout (plus stderr),
/// so the line-based filters see real newlines rather than escaped JSON.
fn extract_content(call: &ToolCall, output: &Value) -> Option<String> {
    if call.name == "bash" {
        return Some(bash_content(output));
    }
    match output {
        Value::String(s) => Some(s.clone()),
        other => serde_json::to_string_pretty(other).ok(),
    }
}

fn bash_content(output: &Value) -> String {
    let stdout = output
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = output
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if stderr.is_empty() {
        stdout.to_owned()
    } else {
        format!("{stdout}\n----- stderr -----\n{stderr}")
    }
}

fn bash_command(call: &ToolCall) -> Option<&str> {
    if call.name != "bash" {
        return None;
    }
    call.args.get("command").and_then(Value::as_str)
}

fn bash_exit_code(call: &ToolCall, output: &Value) -> Option<i32> {
    if call.name != "bash" {
        return None;
    }
    output
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
}

/// Stringify an output value verbatim (used by the uncertain-marker path so
/// nothing, including a bash exit code, is lost).
fn stringify_output(output: &Value) -> String {
    match output {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    use crabgent_core::Subject;
    use crabgent_core::run_id::RunId;
    use crabgent_store::memory::MemoryToolCacheStore;
    use crabgent_store::{SessionId, StoreError, ToolCacheEntry, ToolCacheStore};
    use serde_json::json;

    use crate::session::default_session_id;

    /// A store whose insert always fails, to prove the fail-open passthrough.
    struct FailingStore;

    #[async_trait]
    impl ToolCacheStore for FailingStore {
        async fn insert(&self, _entry: &ToolCacheEntry) -> Result<(), StoreError> {
            Err(StoreError::Transient("insert down".into()))
        }

        async fn get(
            &self,
            _id: &str,
            _session_id: &SessionId,
        ) -> Result<Option<ToolCacheEntry>, StoreError> {
            Ok(None)
        }

        async fn cleanup_expired(&self) -> Result<u64, StoreError> {
            Ok(0)
        }
    }

    fn hook(store: Arc<MemoryToolCacheStore>) -> ToolCompactHook<MemoryToolCacheStore> {
        ToolCompactHook::new(store, ToolCompactConfig::default().with_min_tokens(1))
    }

    fn ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("user-1"))
    }

    fn bash_call(command: &str) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            args: json!({ "command": command }),
            thought_signature: None,
        }
    }

    fn bash_result(stdout: &str, exit_code: i32) -> ToolResult {
        ToolResult::success(json!({
            "stdout": stdout,
            "stderr": "",
            "exit_code": exit_code,
            "timed_out": false
        }))
        .with_call_id("c1")
    }

    #[tokio::test]
    async fn compact_stash_replace_roundtrip() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let h = hook(Arc::clone(&store));
        let ctx = ctx();
        let mut stdout = String::new();
        for i in 0..40 {
            writeln!(stdout, "test case_{i} ... ok").expect("write to string");
        }
        stdout.push_str("test result: ok. 40 passed; 0 failed");
        let result = bash_result(&stdout, 0);

        let decision = h.after_tool(&bash_call("cargo test"), &result, &ctx).await;
        let Decision::Replace(replaced) = decision else {
            panic!("expected Replace");
        };
        let text = replaced.output.as_str().expect("string output");
        assert!(text.contains("test result: ok"));
        assert!(text.contains("recall:"));
        assert!(text.contains("compacted"));

        // the full stdout is recoverable from the stash.
        let handle = RecallHandle::new(&ctx.run_id, &stdout);
        let entry = store
            .get(&handle.to_string(), &default_session_id(&ctx.subject))
            .await
            .expect("get")
            .expect("entry present");
        assert_eq!(entry.content, stdout);
    }

    #[tokio::test]
    async fn recall_tool_name_skipped() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let h = hook(store);
        let call = ToolCall {
            id: "c2".into(),
            name: RECALL_TOOL_NAME.into(),
            args: json!({}),
            thought_signature: None,
        };
        let result = ToolResult::success(Value::String("x".repeat(9000))).with_call_id("c2");
        assert!(matches!(
            h.after_tool(&call, &result, &ctx()).await,
            Decision::Continue
        ));
    }

    #[tokio::test]
    async fn small_output_passes_through() {
        let store = Arc::new(MemoryToolCacheStore::default());
        // default threshold (4096 tokens); a short output stays inline.
        let h = ToolCompactHook::new(store, ToolCompactConfig::default());
        let decision = h
            .after_tool(&bash_call("cargo test"), &bash_result("ok", 0), &ctx())
            .await;
        assert!(matches!(decision, Decision::Continue));
    }

    #[tokio::test]
    async fn uncertain_marker_does_not_stash() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let h = hook(Arc::clone(&store));
        let ctx = ctx();
        // exit 1 but body claims success: conflict -> marker, no stash.
        let stdout = format!(
            "{}\nsummary: 0 failed, everything fine",
            "line\n".repeat(40)
        );
        let result = bash_result(&stdout, 1);
        let decision = h.after_tool(&bash_call("cargo test"), &result, &ctx).await;
        let Decision::Replace(replaced) = decision else {
            panic!("expected Replace");
        };
        let text = replaced.output.as_str().expect("string");
        assert!(text.starts_with(UNCERTAIN_MARKER));
        // The model sees human-readable stdout with real newlines, not escaped
        // JSON: the body lines and summary survive verbatim.
        assert!(text.contains("summary: 0 failed, everything fine"));
        assert!(
            text.contains("line\nline"),
            "real newlines, not \\n: {text}"
        );
        assert!(!text.contains("\"stdout\""), "no JSON envelope: {text}");
        // nothing was stashed.
        let handle = RecallHandle::new(&ctx.run_id, &stdout);
        assert!(
            store
                .get(&handle.to_string(), &default_session_id(&ctx.subject))
                .await
                .expect("get")
                .is_none()
        );
    }

    #[tokio::test]
    async fn on_stop_clears_autodisable() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let tracker = AutoDisableTracker::new();
        let h = hook(store).with_tracker(tracker.clone());
        let ctx = ctx();
        for _ in 0..3 {
            tracker.record(&ctx.run_id, "bash").await;
        }
        assert!(tracker.is_disabled(&ctx.run_id, "bash", 3).await);
        h.on_stop(&ctx, &Outcome::Completed(String::new())).await;
        assert!(!tracker.is_disabled(&ctx.run_id, "bash", 3).await);
    }

    #[tokio::test]
    async fn stash_insert_failure_is_fail_open() {
        // A compactable output that would normally be stashed + replaced, but
        // the store insert fails. The hook leaves the raw output inline
        // (Decision::Continue) rather than dropping the original.
        let h = ToolCompactHook::new(
            Arc::new(FailingStore),
            ToolCompactConfig::default().with_min_tokens(1),
        );
        let ctx = ctx();
        let mut stdout = String::new();
        for i in 0..40 {
            writeln!(stdout, "test case_{i} ... ok").expect("write to string");
        }
        stdout.push_str("test result: ok. 40 passed; 0 failed");
        let result = bash_result(&stdout, 0);

        let decision = h.after_tool(&bash_call("cargo test"), &result, &ctx).await;
        assert!(
            matches!(decision, Decision::Continue),
            "store failure leaves the raw output inline"
        );
    }

    #[tokio::test]
    async fn autodisabled_tool_passes_through() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let tracker = AutoDisableTracker::new();
        let h = hook(Arc::clone(&store)).with_tracker(tracker.clone());
        let ctx = ctx();
        for _ in 0..3 {
            tracker.record(&ctx.run_id, "bash").await;
        }
        let stdout = "test result: ok. 9 passed; 0 failed\n".repeat(30);
        let decision = h
            .after_tool(&bash_call("cargo test"), &bash_result(&stdout, 0), &ctx)
            .await;
        assert!(matches!(decision, Decision::Continue));
    }
}
