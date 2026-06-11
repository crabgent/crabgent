//! Memory search operation.

use crabgent_core::error::ToolError;
use crabgent_core::tool::{ToolCtx, clamp_positive_limit};
use crabgent_core::{Action, MAX_SEARCH_LIMIT, MemoryScope, SearchQuery};
use crabgent_memory::{MemoryClass, MemoryError, MemoryRecall};
use crabgent_store::MemoryHit;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::MemoryTool;
use crate::ops::Args;
use crate::store_unavailable;

pub async fn do_search(
    tool: &MemoryTool,
    args: &Args,
    scope: MemoryScope,
    ctx: &ToolCtx,
) -> Result<Value, ToolError> {
    let query_text = args.query.as_deref().unwrap_or_default();
    let action = Action::MemorySearch {
        query: query_text.to_owned(),
        scope: scope.clone(),
    };
    tool.gate(&action, ctx).await?;

    let mut q = SearchQuery::new(query_text).scope(scope);
    if let Some(s) = args.since {
        q = q.since(s);
    }
    if let Some(u) = args.until {
        q = q.until(u);
    }
    if let Some(class) = args.class.as_deref() {
        let class = MemoryClass::from_str(class)
            .map_err(|err| ToolError::InvalidArgs(format!("class: {err}")))?;
        q = q.class(class.as_str());
    }
    if args.include_expired.unwrap_or(false) {
        q = q.include_expired();
    }
    if args.include_archived.unwrap_or(false) {
        q = q.include_archived();
    }
    if let Some(l) = args.limit {
        q = q.limit(clamp_positive_limit(l, MAX_SEARCH_LIMIT, "memory.search")?);
    }
    if let Some(o) = args.offset {
        q = q.offset(o);
    }
    if let Some(embedding) = tool.embed_text("memory.search", query_text, ctx).await? {
        q = q.embedding(embedding);
    }
    let hits = MemoryRecall::new(tool.store.clone())
        .search(&q)
        .await
        .map_err(recall_unavailable)?;
    Ok(json!({
        "count": hits.len(),
        "hits": hits.iter().map(hit_to_json).collect::<Vec<_>>()
    }))
}

fn recall_unavailable(err: MemoryError) -> ToolError {
    match err {
        MemoryError::Store(err) => store_unavailable("memory.search", &err),
        _ => ToolError::Execution("memory.search: recall unavailable".to_owned()),
    }
}

fn hit_to_json(hit: &MemoryHit) -> Value {
    json!({
        "id": hit.id.to_string(),
        "body": hit.body,
        "score": hit.score,
        "created_at": hit.created_at.to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use crabgent_core::policy::{AllowAllPolicy, DenyAllPolicy};
    use crabgent_core::{
        EmbeddingError, MemoryId, MemoryScope, Owner, SearchQuery, Tool, ToolError,
    };
    use crabgent_store::{MemoryDoc, MemoryHit, MemoryStore, StoreError};
    use serde_json::json;

    use crate::ops::test_support::{
        FailingEmbeddingProvider, FixedEmbeddingProvider, alice_ctx, alice_scope,
        alice_scope_value, allow_all_tool, make_tool,
    };

    #[derive(Default)]
    struct RecordingStore {
        last_query: Mutex<Option<SearchQuery>>,
    }

    #[async_trait]
    impl MemoryStore for RecordingStore {
        async fn search(&self, query: &SearchQuery) -> Result<Vec<MemoryHit>, StoreError> {
            *self
                .last_query
                .lock()
                .expect("mutex should not be poisoned") = Some(query.clone());
            Ok(Vec::new())
        }

        async fn store(&self, doc: &MemoryDoc) -> Result<MemoryId, StoreError> {
            Ok(doc.id.clone())
        }

        async fn get(&self, _id: &MemoryId) -> Result<Option<MemoryDoc>, StoreError> {
            Ok(None)
        }

        async fn delete(&self, _id: &MemoryId) -> Result<bool, StoreError> {
            Ok(false)
        }

        async fn delete_scoped(
            &self,
            _id: &MemoryId,
            _scope: &MemoryScope,
        ) -> Result<bool, StoreError> {
            Ok(false)
        }

        async fn update_body(&self, _id: &MemoryId, _new_body: String) -> Result<bool, StoreError> {
            Ok(false)
        }

        async fn update_body_with_embedding(
            &self,
            _id: &MemoryId,
            _new_body: String,
            _embedding: Option<Vec<f32>>,
        ) -> Result<bool, StoreError> {
            Ok(false)
        }
    }

    fn recorded_query(store: &RecordingStore) -> SearchQuery {
        store
            .last_query
            .lock()
            .expect("mutex should not be poisoned")
            .clone()
            .expect("search should record query")
    }

    #[tokio::test]
    async fn memory_tool_rejects_args_without_scope() {
        let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
        let args = json!({"op": "search", "query": "x"});
        let err = tool
            .execute(args, &alice_ctx())
            .await
            .expect_err("expected error");
        assert!(
            matches!(err, ToolError::InvalidArgs(msg) if msg.contains("missing field `scope`"))
        );
    }

    #[tokio::test]
    async fn deny_all_blocks_search() {
        let (tool, _) = make_tool(Arc::new(DenyAllPolicy));
        let args = json!({
            "op": "search",
            "scope": alice_scope_value(),
            "query": "x"
        });
        let err = tool
            .execute(args, &alice_ctx())
            .await
            .expect_err("expected error");
        assert!(matches!(err, ToolError::Permission(_)));
    }

    #[tokio::test]
    async fn search_filters_expired_default() {
        let (tool, store) = allow_all_tool();
        let mut doc = MemoryDoc::new(alice_scope(), "old episodic note");
        doc.expires_at = Some(chrono::Utc::now() - chrono::Duration::hours(1));
        store.store(&doc).await.expect("test result");

        let result = tool
            .execute(
                json!({
                    "op": "search",
                    "scope": alice_scope_value(),
                    "query": "old"
                }),
                &alice_ctx(),
            )
            .await
            .expect("test result");

        assert_eq!(result["count"], 0);
    }

    #[tokio::test]
    async fn search_with_include_expired_returns() {
        let (tool, store) = allow_all_tool();
        let mut doc = MemoryDoc::new(alice_scope(), "old episodic note");
        doc.expires_at = Some(chrono::Utc::now() - chrono::Duration::hours(1));
        store.store(&doc).await.expect("test result");

        let result = tool
            .execute(
                json!({
                    "op": "search",
                    "scope": alice_scope_value(),
                    "query": "old",
                    "include_expired": true
                }),
                &alice_ctx(),
            )
            .await
            .expect("test result");

        assert_eq!(result["count"], 1);
    }

    #[tokio::test]
    async fn search_with_class_filter() {
        let (tool, store) = allow_all_tool();
        let mut semantic = MemoryDoc::new(alice_scope(), "shared needle semantic");
        semantic.class = Some("semantic".to_owned());
        store.store(&semantic).await.expect("test result");
        let mut episodic = MemoryDoc::new(
            MemoryScope::for_owner(Owner::new("alice")),
            "shared needle episodic",
        );
        episodic.class = Some("episodic".to_owned());
        store.store(&episodic).await.expect("test result");

        let result = tool
            .execute(
                json!({
                    "op": "search",
                    "scope": alice_scope_value(),
                    "query": "shared needle",
                    "class": "semantic"
                }),
                &alice_ctx(),
            )
            .await
            .expect("test result");

        assert_eq!(result["count"], 1);
        assert_eq!(result["hits"][0]["body"], "shared needle semantic");
    }

    #[tokio::test]
    async fn search_embeds_query_when_provider_is_configured() {
        let store = Arc::new(RecordingStore::default());
        let store_dyn: Arc<dyn MemoryStore> = store.clone();
        let provider = Arc::new(FixedEmbeddingProvider::new(vec![0.25, 0.5, 1.0]));
        let tool = crate::MemoryTool::new(store_dyn, Arc::new(AllowAllPolicy), Some(provider));

        tool.execute(
            json!({
                "op": "search",
                "scope": alice_scope_value(),
                "query": "embedded query"
            }),
            &alice_ctx(),
        )
        .await
        .expect("test result");

        assert_eq!(recorded_query(&store).embedding, Some(vec![0.25, 0.5, 1.0]));
    }

    #[tokio::test]
    async fn search_falls_back_to_fts_when_embedding_fails() {
        let store = Arc::new(RecordingStore::default());
        let store_dyn: Arc<dyn MemoryStore> = store.clone();
        let provider = Arc::new(FailingEmbeddingProvider::new(EmbeddingError::Other(
            "offline".to_owned(),
        )));
        let tool = crate::MemoryTool::new(store_dyn, Arc::new(AllowAllPolicy), Some(provider));

        tool.execute(
            json!({
                "op": "search",
                "scope": alice_scope_value(),
                "query": "embedded query"
            }),
            &alice_ctx(),
        )
        .await
        .expect("test result");

        assert_eq!(recorded_query(&store).embedding, None);
    }
}
