//! [`RecallTool`]: retrieve a stashed full tool output by handle.
//!
//! Two ops: `recall_raw` reads from an offset, `expand` reads a `[start, end)`
//! region. Both cap the bytes returned per call (default 4 KiB, max 32 KiB)
//! and paginate via `total_size`/`has_more`, so the model cannot pull a full
//! payload back into the session and neutralize the compaction. Each call
//! records an expansion against the originating run and the original tool name
//! (recovered from the handle and the stash entry) for auto-disable.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::text::floor_char_boundary;
use crabgent_core::tool::{
    Tool, ToolCtx, clamp_optional_usize_limit, gate_tool_action, parse_args_with_context,
};
use crabgent_core::{Action, PolicyHook, Subject};
use crabgent_store::{SessionId, ToolCacheStore};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::autodisable::AutoDisableTracker;
use crate::config::{DEFAULT_RECALL_LIMIT, MAX_RECALL_LIMIT};
use crate::handle::RecallHandle;
use crate::session::{SessionResolver, default_session_resolver};

/// The LLM-facing name of the recall tool.
pub const RECALL_TOOL_NAME: &str = "recall";

const DESCRIPTION: &str = "Retrieve the full output that tool-output compaction \
    stashed, by the handle shown in a compacted block's footer (recall: <handle>). \
    op=recall_raw reads from a byte offset; op=expand reads a [start, end) byte \
    region. Indices are UTF-8-safe. Returns at most 32 KiB per call; paginate via \
    offset and the has_more flag.";

/// Tool that resolves a [`RecallHandle`] back to (a slice of) its full content.
pub struct RecallTool<C: ToolCacheStore> {
    store: Arc<C>,
    policy: Arc<dyn PolicyHook>,
    resolve_session: SessionResolver,
    tracker: AutoDisableTracker,
    default_limit: usize,
    max_limit: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Op {
    RecallRaw,
    Expand,
}

#[derive(Deserialize)]
struct Args {
    op: Op,
    handle: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    start: Option<u64>,
    #[serde(default)]
    end: Option<u64>,
}

impl<C: ToolCacheStore> RecallTool<C> {
    /// Build a recall tool sharing the store, policy, and auto-disable tracker.
    pub fn new(store: Arc<C>, policy: Arc<dyn PolicyHook>, tracker: AutoDisableTracker) -> Self {
        Self {
            store,
            policy,
            resolve_session: default_session_resolver(),
            tracker,
            default_limit: DEFAULT_RECALL_LIMIT,
            max_limit: MAX_RECALL_LIMIT,
        }
    }

    /// Use a shared session resolver (same as the hook).
    #[must_use]
    pub fn with_shared_session_resolver(mut self, resolver: SessionResolver) -> Self {
        self.resolve_session = resolver;
        self
    }

    /// Override the default and maximum per-call byte caps.
    #[must_use]
    pub const fn with_limits(mut self, default_limit: usize, max_limit: usize) -> Self {
        self.default_limit = default_limit;
        self.max_limit = max_limit;
        self
    }

    fn resolve(&self, subject: &Subject) -> SessionId {
        (self.resolve_session)(subject)
    }

    async fn gate(&self, ctx: &ToolCtx) -> Result<(), ToolError> {
        gate_tool_action(self.policy.as_ref(), ctx, &Action::tool(RECALL_TOOL_NAME)).await
    }
}

/// The byte offset and requested limit for one op.
fn offset_and_limit(args: &Args) -> (u64, Option<u64>) {
    match args.op {
        Op::RecallRaw => (args.offset.unwrap_or(0), args.limit),
        Op::Expand => {
            let start = args.start.unwrap_or(0);
            let end = args.end.unwrap_or(start);
            (start, Some(end.saturating_sub(start)))
        }
    }
}

#[async_trait]
impl<C> Tool for RecallTool<C>
where
    C: ToolCacheStore + 'static,
{
    fn name(&self) -> &'static str {
        RECALL_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "op": {"type": "string", "enum": ["recall_raw", "expand"]},
                "handle": {"type": "string", "description": "Recall handle from a compacted footer"},
                "offset": {"type": "integer", "minimum": 0, "description": "recall_raw: byte offset (default 0)"},
                "limit": {"type": "integer", "minimum": 0, "description": "recall_raw: max bytes (default 4 KiB, max 32 KiB)"},
                "start": {"type": "integer", "minimum": 0, "description": "expand: region start byte"},
                "end": {"type": "integer", "minimum": 0, "description": "expand: region end byte"}
            },
            "required": ["op", "handle"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        self.gate(ctx).await?;
        let parsed: Args = parse_args_with_context(args, "recall args")?;
        let handle = RecallHandle::from_str(&parsed.handle)
            .map_err(|e| ToolError::InvalidArgs(format!("invalid recall handle: {e}")))?;

        let session_id = self.resolve(&ctx.subject);
        let entry = self
            .store
            .get(&handle.to_string(), &session_id)
            .await
            .map_err(|err| {
                crabgent_log::warn!(
                    op = "recall.get",
                    error_kind = err.kind(),
                    transient = err.is_transient(),
                    "tool cache store unavailable"
                );
                ToolError::backend_unavailable("recall.get", &err)
            })?
            .ok_or_else(|| {
                ToolError::NotFound(format!("recall handle {} not found", parsed.handle))
            })?;

        // Count this recall against the originating run + the original tool.
        // The run is read from the handle, not from ctx (ToolCtx carries no
        // run id). In the normal case the model recalls a handle from the
        // current run, so this keys the current run. A handle reused from a
        // prior run within the same session would credit that prior run
        // instead; auto-disable is a best-effort heuristic, so this edge is
        // accepted rather than guarded.
        self.tracker.record(handle.run_id(), &entry.tool_name).await;

        let (offset, limit_arg) = offset_and_limit(&parsed);
        let total = entry.content.len();
        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        let start = floor_char_boundary(&entry.content, offset.min(total));
        let limit = clamp_optional_usize_limit(limit_arg, self.default_limit, self.max_limit);
        let end = floor_char_boundary(&entry.content, start.saturating_add(limit).min(total));
        let slice = entry.content.get(start..end).unwrap_or_default();

        Ok(json!({
            "handle": parsed.handle,
            "tool": entry.tool_name,
            "content": slice,
            "total_size": total,
            "offset": start,
            "has_more": end < total
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use crabgent_core::run_id::RunId;
    use crabgent_core::{AllowAllPolicy, DenyAllPolicy};
    use crabgent_store::ToolCacheEntry;
    use crabgent_store::memory::MemoryToolCacheStore;

    use crate::session::default_session_id;

    fn tool(
        store: Arc<MemoryToolCacheStore>,
        tracker: AutoDisableTracker,
    ) -> RecallTool<MemoryToolCacheStore> {
        RecallTool::new(store, Arc::new(AllowAllPolicy), tracker)
    }

    async fn seed(
        store: &MemoryToolCacheStore,
        run: &RunId,
        subject: &Subject,
        content: &str,
    ) -> String {
        let handle = RecallHandle::new(run, content);
        let entry = ToolCacheEntry {
            id: handle.to_string(),
            session_id: default_session_id(subject),
            tool_name: "bash".into(),
            content: content.to_owned(),
            preview: "...".into(),
            created_at: Utc::now(),
            expires_at: Utc::now() + Duration::hours(1),
        };
        store.insert(&entry).await.expect("seed insert");
        handle.to_string()
    }

    fn ctx(subject: &Subject) -> ToolCtx {
        ToolCtx::new(subject.clone())
    }

    #[tokio::test]
    async fn recall_raw_slices_from_offset() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let run = RunId::new();
        let subject = Subject::new("user-1");
        let handle = seed(&store, &run, &subject, "0123456789").await;
        let t = tool(Arc::clone(&store), AutoDisableTracker::new());
        let res = t
            .execute(
                json!({"op": "recall_raw", "handle": handle, "offset": 3, "limit": 4}),
                &ctx(&subject),
            )
            .await
            .expect("ok");
        assert_eq!(res["content"], "3456");
        assert_eq!(res["total_size"], 10);
        assert_eq!(res["has_more"], true);
    }

    #[tokio::test]
    async fn expand_reads_region() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let run = RunId::new();
        let subject = Subject::new("user-1");
        let handle = seed(&store, &run, &subject, "abcdefghij").await;
        let t = tool(Arc::clone(&store), AutoDisableTracker::new());
        let res = t
            .execute(
                json!({"op": "expand", "handle": handle, "start": 2, "end": 5}),
                &ctx(&subject),
            )
            .await
            .expect("ok");
        assert_eq!(res["content"], "cde");
    }

    #[tokio::test]
    async fn recall_unknown_handle_is_not_found() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let subject = Subject::new("user-1");
        let handle = RecallHandle::new(&RunId::new(), "absent").to_string();
        let t = tool(store, AutoDisableTracker::new());
        let err = t
            .execute(
                json!({"op": "recall_raw", "handle": handle}),
                &ctx(&subject),
            )
            .await
            .expect_err("not found");
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    #[tokio::test]
    async fn recall_bad_handle_is_invalid_args() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let subject = Subject::new("user-1");
        let t = tool(store, AutoDisableTracker::new());
        let err = t
            .execute(
                json!({"op": "recall_raw", "handle": "not-a-handle"}),
                &ctx(&subject),
            )
            .await
            .expect_err("invalid");
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn recall_policy_deny_blocks() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let run = RunId::new();
        let subject = Subject::new("user-1");
        let handle = seed(&store, &run, &subject, "secretdata content here").await;
        let t = RecallTool::new(store, Arc::new(DenyAllPolicy), AutoDisableTracker::new());
        let err = t
            .execute(
                json!({"op": "recall_raw", "handle": handle}),
                &ctx(&subject),
            )
            .await
            .expect_err("deny");
        assert!(matches!(err, ToolError::Permission(_)));
    }

    #[tokio::test]
    async fn expansion_recorded_for_original_tool() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let run = RunId::new();
        let subject = Subject::new("user-1");
        let handle = seed(&store, &run, &subject, "payload data here longer").await;
        let tracker = AutoDisableTracker::new();
        let t = tool(Arc::clone(&store), tracker.clone());
        for _ in 0..3 {
            t.execute(
                json!({"op": "recall_raw", "handle": handle}),
                &ctx(&subject),
            )
            .await
            .expect("ok");
        }
        // the entry's tool_name is "bash"; three recalls should trip n=3.
        assert!(tracker.is_disabled(&run, "bash", 3).await);
    }

    #[tokio::test]
    async fn recall_clamps_limit_to_max() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let run = RunId::new();
        let subject = Subject::new("user-1");
        let big = "x".repeat(40_000);
        let handle = seed(&store, &run, &subject, &big).await;
        let t = tool(Arc::clone(&store), AutoDisableTracker::new());
        let res = t
            .execute(
                json!({"op": "recall_raw", "handle": handle, "limit": 99_999_999_u64}),
                &ctx(&subject),
            )
            .await
            .expect("ok");
        let content = res["content"].as_str().expect("string");
        assert_eq!(content.len(), MAX_RECALL_LIMIT);
        assert_eq!(res["has_more"], true);
    }
}
