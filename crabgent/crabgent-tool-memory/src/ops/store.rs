//! Memory store operation.

use crabgent_core::error::ToolError;
use crabgent_core::tool::ToolCtx;
use crabgent_core::{Action, MemoryScope};
use crabgent_memory::{MemoryClass, MemoryImportance};
use crabgent_store::MemoryDoc;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::MemoryTool;
use crate::ops::Args;
use crate::store_unavailable;

pub async fn do_store(
    tool: &MemoryTool,
    args: &Args,
    scope: MemoryScope,
    ctx: &ToolCtx,
) -> Result<Value, ToolError> {
    let body = args
        .body
        .as_deref()
        .ok_or_else(|| ToolError::InvalidArgs("body required for op=store".into()))?;
    let action = Action::MemoryStore {
        scope: scope.clone(),
    };
    tool.gate(&action, ctx).await?;
    let class = args
        .class
        .as_deref()
        .map(MemoryClass::from_str)
        .transpose()
        .map_err(|err| ToolError::InvalidArgs(format!("class: {err}")))?;
    let importance = args
        .importance
        .map(MemoryImportance::new)
        .transpose()
        .map_err(|err| ToolError::InvalidArgs(format!("importance: {err}")))?;
    let mut doc = MemoryDoc::new(scope, body);
    doc.class = class.map(|class| class.as_str().to_owned());
    doc.importance = importance.map(MemoryImportance::into_inner);
    doc.expires_at = args.expires_at;
    doc.embedding = tool.embed_text("memory.store", body, ctx).await?;
    let id = tool
        .store
        .store(&doc)
        .await
        .map_err(|err| store_unavailable("memory.store", &err))?;
    Ok(json!({ "id": id.to_string(), "stored": true }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crabgent_core::EmbeddingError;
    use crabgent_core::Tool;
    use crabgent_core::policy::AllowAllPolicy;
    use crabgent_store::MemoryStore;
    use serde_json::json;

    use crate::ops::test_support::{
        FailingEmbeddingProvider, FixedEmbeddingProvider, alice_ctx, alice_scope,
        alice_scope_value, allow_all_tool, make_tool_with_embedding_provider,
    };

    #[tokio::test]
    async fn store_then_search_round_trip_with_allow_all() {
        let (tool, _) = allow_all_tool();
        let store_args = json!({
            "op": "store",
            "scope": alice_scope_value(),
            "body": "remember this fact"
        });
        let stored = tool
            .execute(store_args, &alice_ctx())
            .await
            .expect("test result");
        assert_eq!(stored["stored"], true);

        let search_args = json!({
            "op": "search",
            "scope": alice_scope_value(),
            "query": "remember"
        });
        let result = tool
            .execute(search_args, &alice_ctx())
            .await
            .expect("test result");
        assert_eq!(result["count"], 1);
        assert_eq!(result["hits"][0]["body"], "remember this fact");
    }

    #[tokio::test]
    async fn store_with_episodic_class_persists() {
        let (tool, store) = allow_all_tool();
        let stored = tool
            .execute(
                json!({
                    "op": "store",
                    "scope": alice_scope_value(),
                    "body": "episodic event",
                    "class": "episodic",
                    "importance": 0.8
                }),
                &alice_ctx(),
            )
            .await
            .expect("test result");
        let id = stored["id"]
            .as_str()
            .expect("value should be a string")
            .parse()
            .expect("value should parse");
        let doc = store
            .get(&id)
            .await
            .expect("test result")
            .expect("test result");

        assert_eq!(doc.class.as_deref(), Some("episodic"));
        assert_eq!(doc.importance, Some(0.8));
    }

    #[tokio::test]
    async fn store_invalid_class_errors() {
        let (tool, _) = allow_all_tool();
        let err = tool
            .execute(
                json!({
                    "op": "store",
                    "scope": alice_scope_value(),
                    "body": "bad class",
                    "class": "working"
                }),
                &alice_ctx(),
            )
            .await
            .expect_err("expected error");

        assert!(matches!(err, crabgent_core::ToolError::InvalidArgs(msg) if msg.contains("class")));
    }

    #[tokio::test]
    async fn store_invalid_importance_errors() {
        let (tool, _) = allow_all_tool();
        let err = tool
            .execute(
                json!({
                    "op": "store",
                    "scope": alice_scope_value(),
                    "body": "too important",
                    "importance": 1.2
                }),
                &alice_ctx(),
            )
            .await
            .expect_err("expected error");

        assert!(
            matches!(err, crabgent_core::ToolError::InvalidArgs(msg) if msg.contains("importance"))
        );
    }

    #[tokio::test]
    async fn store_without_class_compat() {
        let (tool, store) = allow_all_tool();
        let stored = tool
            .execute(
                json!({
                    "op": "store",
                    "scope": alice_scope_value(),
                    "body": "plain old memory"
                }),
                &alice_ctx(),
            )
            .await
            .expect("test result");
        let id = stored["id"]
            .as_str()
            .expect("value should be a string")
            .parse()
            .expect("value should parse");
        let doc = store
            .get(&id)
            .await
            .expect("test result")
            .expect("test result");

        assert!(doc.class.is_none());
        assert_eq!(doc.scope, alice_scope());
    }

    #[tokio::test]
    async fn store_embeds_body_when_provider_is_configured() {
        let provider = Arc::new(FixedEmbeddingProvider::new(vec![0.25, 0.5, 1.0]));
        let (tool, store) = make_tool_with_embedding_provider(Arc::new(AllowAllPolicy), provider);
        let stored = tool
            .execute(
                json!({
                    "op": "store",
                    "scope": alice_scope_value(),
                    "body": "embedded memory"
                }),
                &alice_ctx(),
            )
            .await
            .expect("test result");
        let id = stored["id"]
            .as_str()
            .expect("value should be a string")
            .parse()
            .expect("value should parse");
        let doc = store
            .get(&id)
            .await
            .expect("test result")
            .expect("test result");

        assert_eq!(doc.embedding, Some(vec![0.25, 0.5, 1.0]));
    }

    #[tokio::test]
    async fn store_falls_back_to_fts_when_embedding_fails() {
        let provider = Arc::new(FailingEmbeddingProvider::new(EmbeddingError::Other(
            "offline".to_owned(),
        )));
        let (tool, store) = make_tool_with_embedding_provider(Arc::new(AllowAllPolicy), provider);
        let stored = tool
            .execute(
                json!({
                    "op": "store",
                    "scope": alice_scope_value(),
                    "body": "plain memory"
                }),
                &alice_ctx(),
            )
            .await
            .expect("test result");
        let id = stored["id"]
            .as_str()
            .expect("value should be a string")
            .parse()
            .expect("value should parse");
        let doc = store
            .get(&id)
            .await
            .expect("test result")
            .expect("test result");

        assert_eq!(doc.embedding, None);
    }
}
