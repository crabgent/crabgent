//! [`CacheReadTool`]: a [`crabgent_core::tool::Tool`] that retrieves
//! previously cached oversized tool output by id.
//!
//! The LLM gets the cache id from the [`crate::TruncatedOutput`] object that
//! [`crate::ToolCacheHook`] injects into compacted results, and invokes this
//! tool to read the full content (or a slice).

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

use crate::resolver::{SessionResolver, default_session_resolver};

const TOOL_NAME: &str = "cache_read";
/// Default byte cap for a `cache_read` slice when the caller omits `limit`.
pub const DEFAULT_CACHE_READ_LIMIT: usize = 4 * 1024;
/// Maximum byte cap the `cache_read` tool will ever return in a single call.
/// Bigger payloads must paginate via `offset`/`limit` or use the
/// `TruncatedOutput.hint` task-spawn path; the cap exists so the LLM cannot
/// pull a full cached payload back into the session and neutralize the cache.
pub const MAX_CACHE_READ_LIMIT: usize = 32 * 1024;

const DESCRIPTION: &str = "Retrieve previously cached tool output by id. \
     Use this when a tool result shows a cached output object whose \
     cache_id field contains an id. Supports byte-range slicing via offset and limit \
     (both byte counts; UTF-8-safe, indices are floored to char boundaries; \
     default limit 4 KiB, max 32 KiB).";

/// Tool that resolves a cached output id back to its full content.
///
/// Args (as JSON):
/// - `id`: cache entry id (required)
/// - `offset`: byte offset to start reading from, default 0
/// - `limit`: max bytes to return, default 4 KiB, max 32 KiB
///
/// Result fields:
/// - `id`, `tool`, `content`, `total_size`, `offset`, `has_more`.
///
/// Session scope: by default the tool uses the same subject-based
/// resolver as [`crate::ToolCacheHook`].
pub struct CacheReadTool<C: ToolCacheStore> {
    store: Arc<C>,
    policy: Arc<dyn PolicyHook>,
    resolve_session: SessionResolver,
}

#[derive(Deserialize)]
struct Args {
    id: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
}

impl<C: ToolCacheStore> CacheReadTool<C> {
    /// Build a cache reader guarded by the same policy as the owning kernel.
    pub fn new(store: Arc<C>, policy: Arc<dyn PolicyHook>) -> Self {
        Self {
            store,
            policy,
            resolve_session: default_session_resolver(),
        }
    }

    #[must_use]
    pub fn with_shared_session_resolver(mut self, resolver: SessionResolver) -> Self {
        self.resolve_session = resolver;
        self
    }

    fn resolve(&self, subject: &Subject) -> SessionId {
        (self.resolve_session)(subject)
    }
}

#[async_trait]
impl<C> Tool for CacheReadTool<C>
where
    C: ToolCacheStore + 'static,
{
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Cache entry id from the cached output object"
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Byte offset to start reading from (default 0)"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 0,
                    "default": DEFAULT_CACHE_READ_LIMIT,
                    "maximum": MAX_CACHE_READ_LIMIT,
                    "description": "Max bytes to return (default 4 KiB, max 32 KiB)"
                }
            },
            "required": ["id"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        self.gate(ctx).await?;
        let parsed: Args = parse_args_with_context(args, "cache_read args")?;
        let session_id = self.resolve(&ctx.subject);
        let entry = self
            .store
            .get(&parsed.id, &session_id)
            .await
            .map_err(|err| {
                crabgent_log::warn!(
                    op = "cache_read.get",
                    error_kind = err.kind(),
                    transient = err.is_transient(),
                    "tool cache store unavailable"
                );
                ToolError::backend_unavailable("cache_read.get", &err)
            })?
            .ok_or_else(|| ToolError::NotFound(format!("cache entry {} not found", parsed.id)))?;

        let total_size = entry.content.len();
        let offset_in = usize::try_from(parsed.offset.unwrap_or(0)).unwrap_or(usize::MAX);
        let start = floor_char_boundary(&entry.content, offset_in.min(total_size));
        let limit = clamp_optional_usize_limit(
            parsed.limit,
            DEFAULT_CACHE_READ_LIMIT,
            MAX_CACHE_READ_LIMIT,
        );
        let end = floor_char_boundary(&entry.content, start.saturating_add(limit).min(total_size));
        let slice = entry.content.get(start..end).unwrap_or_default();
        Ok(json!({
            "id": parsed.id,
            "tool": entry.tool_name,
            "content": slice,
            "total_size": total_size,
            "offset": start,
            "has_more": end < total_size
        }))
    }
}

impl<C: ToolCacheStore> CacheReadTool<C> {
    async fn gate(&self, ctx: &ToolCtx) -> Result<(), ToolError> {
        let action = Action::tool(TOOL_NAME);
        gate_tool_action(self.policy.as_ref(), ctx, &action).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use crabgent_core::{AllowAllPolicy, DenyAllPolicy};
    use crabgent_store::ToolCacheEntry;
    use crabgent_store::memory::MemoryToolCacheStore;

    fn entry(id: &str, session: &SessionId, content: &str) -> ToolCacheEntry {
        ToolCacheEntry {
            id: id.to_owned(),
            session_id: session.clone(),
            tool_name: "bash".into(),
            content: content.to_owned(),
            preview: "...".into(),
            created_at: Utc::now(),
            expires_at: Utc::now() + Duration::hours(1),
        }
    }

    fn ctx_for(session: &SessionId) -> ToolCtx {
        ToolCtx::new(Subject::new(session.to_string()))
    }

    fn tool(store: Arc<MemoryToolCacheStore>) -> CacheReadTool<MemoryToolCacheStore> {
        CacheReadTool::new(store, Arc::new(AllowAllPolicy))
    }

    #[tokio::test]
    async fn returns_full_content_when_no_offset_or_limit() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let session = SessionId::new();
        store
            .insert(&entry("e1", &session, "full content"))
            .await
            .expect("test result");
        let tool = tool(Arc::clone(&store));
        let res = tool
            .execute(json!({"id": "e1"}), &ctx_for(&session))
            .await
            .expect("test result");
        assert_eq!(res["content"], "full content");
        assert_eq!(res["total_size"], 12);
        assert_eq!(res["offset"], 0);
        assert_eq!(res["has_more"], false);
        assert_eq!(res["tool"], "bash");
    }

    #[tokio::test]
    async fn slices_with_offset_and_limit() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let session = SessionId::new();
        store
            .insert(&entry("e2", &session, "0123456789"))
            .await
            .expect("test result");
        let tool = tool(Arc::clone(&store));
        let res = tool
            .execute(
                json!({"id": "e2", "offset": 3, "limit": 4}),
                &ctx_for(&session),
            )
            .await
            .expect("test result");
        assert_eq!(res["content"], "3456");
        assert_eq!(res["offset"], 3);
        assert_eq!(res["has_more"], true);
    }

    #[tokio::test]
    async fn offset_beyond_end_yields_empty() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let session = SessionId::new();
        store
            .insert(&entry("e3", &session, "abc"))
            .await
            .expect("test result");
        let tool = tool(Arc::clone(&store));
        let res = tool
            .execute(json!({"id": "e3", "offset": 99}), &ctx_for(&session))
            .await
            .expect("test result");
        assert_eq!(res["content"], "");
        assert_eq!(res["has_more"], false);
        assert_eq!(res["offset"], 3);
    }

    #[tokio::test]
    async fn unknown_id_returns_not_found() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let session = SessionId::new();
        let tool = tool(Arc::clone(&store));
        let err = tool
            .execute(json!({"id": "missing"}), &ctx_for(&session))
            .await
            .expect_err("expected error");
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    #[tokio::test]
    async fn missing_id_arg_is_invalid() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let session = SessionId::new();
        let tool = tool(Arc::clone(&store));
        let err = tool
            .execute(json!({}), &ctx_for(&session))
            .await
            .expect_err("expected error");
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn policy_deny_blocks_cache_read() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let session = SessionId::new();
        store
            .insert(&entry("denied", &session, "secret"))
            .await
            .expect("test result");
        let tool = CacheReadTool::new(Arc::clone(&store), Arc::new(DenyAllPolicy));
        let err = tool
            .execute(json!({"id": "denied"}), &ctx_for(&session))
            .await
            .expect_err("policy deny should block cache read");

        assert!(matches!(err, ToolError::Permission(reason) if reason.contains("DenyAllPolicy")));
    }

    #[tokio::test]
    async fn non_uuid_subject_uses_default_session_resolver() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let subject = Subject::new("not-a-uuid");
        let session = crate::resolver::default_session_id(&subject);
        store
            .insert(&entry("e4", &session, "from-default"))
            .await
            .expect("test result");
        let tool = tool(Arc::clone(&store));
        let ctx = ToolCtx::new(subject);

        let res = tool
            .execute(json!({"id": "e4"}), &ctx)
            .await
            .expect("test result");
        assert_eq!(res["content"], "from-default");
    }

    #[tokio::test]
    async fn custom_resolver_overrides_default() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let session = SessionId::new();
        store
            .insert(&entry("e4", &session, "from-custom"))
            .await
            .expect("test result");
        let target = session.clone();
        let tool = tool(Arc::clone(&store))
            .with_shared_session_resolver(Arc::new(move |_: &Subject| target.clone()));
        let res = tool
            .execute(
                json!({"id": "e4"}),
                &ToolCtx::new(Subject::new("not-a-uuid-but-ok")),
            )
            .await
            .expect("test result");
        assert_eq!(res["content"], "from-custom");
    }

    #[tokio::test]
    async fn cross_session_lookup_misses() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        store
            .insert(&entry("e5", &session_a, "a-only"))
            .await
            .expect("test result");
        let tool = tool(Arc::clone(&store));
        let err = tool
            .execute(json!({"id": "e5"}), &ctx_for(&session_b))
            .await
            .expect_err("expected error");
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    #[tokio::test]
    async fn slice_respects_utf8_boundary() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let session = SessionId::new();
        let smiley = "abc\u{1F600}def";
        store
            .insert(&entry("e6", &session, smiley))
            .await
            .expect("test result");
        let tool = tool(Arc::clone(&store));
        let res = tool
            .execute(
                json!({"id": "e6", "offset": 0, "limit": 4}),
                &ctx_for(&session),
            )
            .await
            .expect("test result");
        assert_eq!(res["content"], "abc");
    }

    #[tokio::test]
    async fn cache_read_clamps_limit_to_max() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let session = SessionId::new();
        let large = "x".repeat(40_000);
        store
            .insert(&entry("e7", &session, &large))
            .await
            .expect("test result");
        let tool = tool(Arc::clone(&store));
        let res = tool
            .execute(
                json!({"id": "e7", "limit": 99_999_999_u64}),
                &ctx_for(&session),
            )
            .await
            .expect("test result");
        let returned = res["content"].as_str().expect("value should be a string");
        assert_eq!(
            returned.len(),
            MAX_CACHE_READ_LIMIT,
            "clamp did not land exactly at MAX_CACHE_READ_LIMIT"
        );
        assert_eq!(res["total_size"], 40_000);
        assert_eq!(res["has_more"], true);
    }

    #[test]
    fn parameters_schema_lists_required_id() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let tool = tool(store);
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"], json!(["id"]));
        assert!(schema["properties"]["id"].is_object());
        assert!(schema["properties"]["offset"].is_object());
        assert!(schema["properties"]["limit"].is_object());
    }

    #[test]
    fn metadata_is_constant() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let tool = tool(store);
        assert_eq!(tool.name(), "cache_read");
        assert!(tool.description().contains("cache_read") || tool.description().contains("cached"));
    }
}
