use std::sync::Arc;

use crabgent_core::{
    AllowAllPolicy, MAX_SEARCH_LIMIT, MemoryScope, Owner, Subject, Tool, ToolCtx, ToolError,
};
use crabgent_store::traits::MemoryStore;
use crabgent_store::{MemoryDoc, MemoryMemoryStore};
use crabgent_tool_memory::MemoryTool;
use serde_json::json;

async fn tool_with_docs(count: u32) -> MemoryTool {
    let store: Arc<MemoryMemoryStore> = Arc::new(MemoryMemoryStore::default());
    let scope = MemoryScope::for_owner(Owner::new("alice"));
    for idx in 0..count {
        store
            .store(&MemoryDoc::new(scope.clone(), format!("needle note {idx}")))
            .await
            .expect("store memory");
    }
    let store_dyn: Arc<dyn MemoryStore> = store;
    MemoryTool::new(store_dyn, Arc::new(AllowAllPolicy), None)
}

#[tokio::test]
async fn search_limit_clamps_to_max() {
    let tool = tool_with_docs(MAX_SEARCH_LIMIT + 50).await;
    let out = tool
        .execute(
            json!({
                "op": "search",
                "scope": {"owner": "alice"},
                "query": "needle",
                "limit": 1_000_000
            }),
            &ToolCtx::new(Subject::new("alice")),
        )
        .await
        .expect("search");

    assert_eq!(out["count"], MAX_SEARCH_LIMIT);
    assert_eq!(
        out["hits"].as_array().expect("hits").len(),
        usize::try_from(MAX_SEARCH_LIMIT).expect("max limit fits usize")
    );
}

#[tokio::test]
async fn zero_limit_is_invalid() {
    let tool = tool_with_docs(1).await;
    let err = tool
        .execute(
            json!({
                "op": "search",
                "scope": {"owner": "alice"},
                "query": "needle",
                "limit": 0
            }),
            &ToolCtx::new(Subject::new("alice")),
        )
        .await
        .expect_err("zero limit");

    assert!(matches!(err, ToolError::InvalidArgs(msg) if msg.contains("limit")));
}
