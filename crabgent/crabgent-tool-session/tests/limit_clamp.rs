use std::sync::Arc;

use chrono::Utc;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, MAX_SEARCH_LIMIT, MemoryScope, Message, Owner, Subject, Tool,
    ToolCtx, ToolError,
};
use crabgent_store::SessionId;
use crabgent_store::memory::MemorySessionStore;
use crabgent_store::records::Session;
use crabgent_store::traits::SessionStore;
use crabgent_tool_session::SessionSearchTool;
use serde_json::json;

async fn tool_with_sessions(count: u32) -> SessionSearchTool {
    let store: Arc<MemorySessionStore> = Arc::new(MemorySessionStore::default());
    for idx in 0..count {
        let now = Utc::now();
        store
            .save(&Session {
                id: SessionId::new(),
                owner: Owner::new("alice"),
                scope: MemoryScope::for_owner(Owner::new("alice")),
                thread: None,
                title: None,
                summary: None,
                compaction_summary: None,
                model_override: None,
                reasoning_effort_override: None,
                messages: vec![Message::User {
                    content: vec![ContentBlock::Text {
                        text: format!("needle session {idx}"),
                    }],
                    timestamp: None,
                }],
                created_at: now,
                updated_at: now,
            })
            .await
            .expect("save session");
    }
    let store_dyn: Arc<dyn SessionStore> = store;
    SessionSearchTool::new(store_dyn, Arc::new(AllowAllPolicy))
}

#[tokio::test]
async fn search_limit_clamps_to_max() {
    let tool = tool_with_sessions(MAX_SEARCH_LIMIT + 50).await;
    let out = tool
        .execute(
            json!({
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
    let tool = tool_with_sessions(1).await;
    let err = tool
        .execute(
            json!({
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
