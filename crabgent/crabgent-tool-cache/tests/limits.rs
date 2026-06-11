use std::sync::Arc;

use chrono::{Duration, Utc};
use crabgent_core::{AllowAllPolicy, Subject, Tool, ToolCtx};
use crabgent_store::memory::MemoryToolCacheStore;
use crabgent_store::{SessionId, ToolCacheEntry, ToolCacheStore};
use crabgent_tool_cache::{CacheReadTool, DEFAULT_CACHE_READ_LIMIT, MAX_CACHE_READ_LIMIT};
use serde_json::json;

fn entry(id: &str, session: &SessionId, content: String) -> ToolCacheEntry {
    ToolCacheEntry {
        id: id.to_owned(),
        session_id: session.clone(),
        tool_name: "bash".into(),
        content,
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
async fn default_limit_is_4_kib() {
    let store = Arc::new(MemoryToolCacheStore::default());
    let session = SessionId::new();
    store
        .insert(&entry(
            "large",
            &session,
            "x".repeat(DEFAULT_CACHE_READ_LIMIT + 1),
        ))
        .await
        .expect("insert");
    let tool = tool(Arc::clone(&store));

    let out = tool
        .execute(json!({"id": "large"}), &ctx_for(&session))
        .await
        .expect("read cache");

    assert_eq!(
        out["content"].as_str().expect("content").len(),
        DEFAULT_CACHE_READ_LIMIT
    );
    assert_eq!(out["has_more"], true);
}

#[tokio::test]
async fn oversized_limit_clamps_to_32_kib() {
    let store = Arc::new(MemoryToolCacheStore::default());
    let session = SessionId::new();
    store
        .insert(&entry(
            "huge",
            &session,
            "x".repeat(MAX_CACHE_READ_LIMIT + 1),
        ))
        .await
        .expect("insert");
    let tool = tool(Arc::clone(&store));

    let out = tool
        .execute(
            json!({"id": "huge", "limit": 1_000_000_000_u64}),
            &ctx_for(&session),
        )
        .await
        .expect("read cache");

    assert_eq!(
        out["content"].as_str().expect("content").len(),
        MAX_CACHE_READ_LIMIT
    );
    assert_eq!(out["has_more"], true);
}
