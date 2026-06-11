//! [`SessionSearchTool`] implementation.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::error::ToolError;
use crabgent_core::tool::{
    Tool, ToolCtx, clamp_positive_limit, gate_tool_action, parse_args_with_context,
};
use crabgent_core::{Action, MAX_SEARCH_LIMIT, MemoryScope, PolicyHook, SearchQuery};
use crabgent_store::StoreError;
use crabgent_store::records::SessionSearchHit;
use crabgent_store::traits::SessionStore;
use serde::Deserialize;
use serde_json::{Value, json};

const TOOL_NAME: &str = "session_search";

const DESCRIPTION: &str = "Search across previously stored conversation \
    sessions. Returns ranked hits with a short excerpt and the session \
    id; fetch the full conversation via a follow-up tool if needed. \
    `scope` is required (set at least `owner`); the policy hook decides \
    which scopes a given subject may search.";

/// LLM-facing session-search tool. Holds store + policy by `Arc`.
pub struct SessionSearchTool {
    store: Arc<dyn SessionStore>,
    policy: Arc<dyn PolicyHook>,
}

impl SessionSearchTool {
    pub fn new(store: Arc<dyn SessionStore>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { store, policy }
    }
}

#[derive(Deserialize)]
struct Args {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    scope: Option<MemoryScope>,
    #[serde(default)]
    since: Option<DateTime<Utc>>,
    #[serde(default)]
    until: Option<DateTime<Utc>>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    offset: Option<u32>,
}

#[async_trait]
impl Tool for SessionSearchTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["scope"],
            "properties": {
                "query": {"type": "string", "description": "Full-text query. Empty/absent lists recent sessions in scope."},
                "scope": crabgent_core::tool::memory_scope_schema(),
                "since": {"type": "string", "format": "date-time"},
                "until": {"type": "string", "format": "date-time"},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT, "description": "Max hits (default 10, max 100)."},
                "offset": {"type": "integer", "minimum": 0}
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let parsed: Args = parse_args_with_context(args, "session_search args")?;
        let scope = parsed
            .scope
            .ok_or_else(|| ToolError::InvalidArgs("scope required".into()))?;
        let query_text = parsed.query.unwrap_or_default();
        let action = Action::SessionSearch {
            query: query_text.clone(),
            scope: scope.clone(),
        };
        gate_tool_action(self.policy.as_ref(), ctx, &action).await?;

        let mut q = SearchQuery::new(&query_text).scope(scope);
        if let Some(s) = parsed.since {
            q = q.since(s);
        }
        if let Some(u) = parsed.until {
            q = q.until(u);
        }
        if let Some(l) = parsed.limit {
            q = q.limit(clamp_positive_limit(l, MAX_SEARCH_LIMIT, "session.search")?);
        }
        if let Some(o) = parsed.offset {
            q = q.offset(o);
        }
        let hits = self
            .store
            .search(&q)
            .await
            .map_err(|err| store_unavailable("session.search", &err))?;
        Ok(json!({
            "count": hits.len(),
            "hits": hits.iter().map(hit_to_json).collect::<Vec<_>>()
        }))
    }
}

fn hit_to_json(hit: &SessionSearchHit) -> Value {
    json!({
        "session_id": hit.session_id.to_string(),
        "excerpt": hit.excerpt,
        "score": hit.score,
        "occurred_at": hit.occurred_at.to_rfc3339(),
    })
}

fn store_unavailable(op: &str, err: &StoreError) -> ToolError {
    crabgent_log::warn!(
        op = %op,
        error_kind = err.kind(),
        transient = err.is_transient(),
        "session store unavailable"
    );
    ToolError::backend_unavailable(op, err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::policy::{AllowAllPolicy, DenyAllPolicy};
    use crabgent_core::{ContentBlock, MemoryScope, Message, Owner, Subject};
    use crabgent_store::SessionId;
    use crabgent_store::memory::MemorySessionStore;
    use crabgent_store::records::Session;

    fn make_tool(policy: Arc<dyn PolicyHook>) -> (SessionSearchTool, Arc<MemorySessionStore>) {
        let store: Arc<MemorySessionStore> = Arc::new(MemorySessionStore::default());
        let store_dyn: Arc<dyn SessionStore> = store.clone();
        (SessionSearchTool::new(store_dyn, policy), store)
    }

    async fn save_user_msg(store: &MemorySessionStore, owner: &str, body: &str) {
        let now = Utc::now();
        let session = Session {
            id: SessionId::new(),
            owner: Owner::new(owner),
            scope: MemoryScope::for_owner(Owner::new(owner)),
            thread: None,
            title: None,
            summary: None,
            compaction_summary: None,
            model_override: None,
            reasoning_effort_override: None,
            messages: vec![Message::User {
                content: vec![ContentBlock::Text {
                    text: body.to_owned(),
                }],
                timestamp: None,
            }],
            created_at: now,
            updated_at: now,
        };
        store.save(&session).await.expect("test result");
    }

    #[tokio::test]
    async fn search_returns_owner_scoped_hits() {
        let (tool, store) = make_tool(Arc::new(AllowAllPolicy));
        save_user_msg(&store, "alice", "the cat is named whiskers").await;
        save_user_msg(&store, "bob", "the cat is named whiskers").await;
        let args = json!({
            "query": "whiskers",
            "scope": {"owner": "alice"}
        });
        let res = tool
            .execute(args, &ToolCtx::new(Subject::new("alice")))
            .await
            .expect("test result");
        assert_eq!(res["count"], 1);
        assert!(
            res["hits"][0]["excerpt"]
                .as_str()
                .expect("test result")
                .to_lowercase()
                .contains("whiskers")
        );
    }

    #[tokio::test]
    async fn search_global_scope_matches_all_owners() {
        let (tool, store) = make_tool(Arc::new(AllowAllPolicy));
        save_user_msg(&store, "alice", "shared phrase").await;
        save_user_msg(&store, "bob", "shared phrase").await;
        let args = json!({
            "query": "shared",
            "scope": {}
        });
        let res = tool
            .execute(args, &ToolCtx::new(Subject::new("admin")))
            .await
            .expect("test result");
        assert_eq!(res["count"], 2);
    }

    #[tokio::test]
    async fn missing_scope_is_invalid_args() {
        let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
        let args = json!({"query": "x"});
        let err = tool
            .execute(args, &ToolCtx::new(Subject::new("alice")))
            .await
            .expect_err("expected error");
        assert!(matches!(err, ToolError::InvalidArgs(msg) if msg.contains("scope required")));
    }

    #[tokio::test]
    async fn deny_all_returns_permission_error() {
        let (tool, _) = make_tool(Arc::new(DenyAllPolicy));
        let args = json!({
            "query": "x",
            "scope": {"owner": "alice"}
        });
        let err = tool
            .execute(args, &ToolCtx::new(Subject::new("alice")))
            .await
            .expect_err("expected error");
        assert!(matches!(err, ToolError::Permission(_)));
    }

    #[tokio::test]
    async fn schema_requires_scope() {
        let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("value should be an array");
        assert!(required.iter().any(|v| v == "scope"));
    }
}
