//! Memory get and delete operations.

use std::str::FromStr;

use crabgent_core::error::ToolError;
use crabgent_core::tool::ToolCtx;
use crabgent_core::{Action, MemoryId, MemoryScope};
use crabgent_store::MemoryDoc;
use serde_json::{Value, json};

use crate::MemoryTool;
use crate::ops::Args;
use crate::store_unavailable;

pub async fn do_get(
    tool: &MemoryTool,
    args: &Args,
    scope: MemoryScope,
    ctx: &ToolCtx,
) -> Result<Value, ToolError> {
    let id = parse_doc_id(args, "get")?;
    let action = Action::MemoryGet {
        id: id.clone(),
        scope: scope.clone(),
    };
    tool.gate(&action, ctx).await?;
    let doc = tool
        .store
        .get(&id)
        .await
        .map_err(|err| store_unavailable("memory.get", &err))?;
    if let Some(doc) = &doc {
        ensure_scope_matches(&scope, &doc.scope, "memory.get")?;
    }
    Ok(json!({ "doc": doc.as_ref().map(doc_to_json) }))
}

pub async fn do_delete(
    tool: &MemoryTool,
    args: &Args,
    scope: MemoryScope,
    ctx: &ToolCtx,
) -> Result<Value, ToolError> {
    let id = parse_doc_id(args, "delete")?;
    let action = Action::MemoryDelete {
        id: id.clone(),
        scope: scope.clone(),
    };
    tool.gate(&action, ctx).await?;
    if let Some(doc) = tool
        .store
        .get(&id)
        .await
        .map_err(|err| store_unavailable("memory.delete.get", &err))?
    {
        ensure_scope_matches(&scope, &doc.scope, "memory.delete")?;
    }
    let removed = tool
        .store
        .delete_scoped(&id, &scope)
        .await
        .map_err(|err| store_unavailable("memory.delete", &err))?;
    Ok(json!({ "deleted": removed }))
}

fn ensure_scope_matches(
    requested: &MemoryScope,
    actual: &MemoryScope,
    op: &str,
) -> Result<(), ToolError> {
    if requested.matches(actual) {
        Ok(())
    } else {
        Err(ToolError::Permission(format!(
            "{op}: document outside requested scope"
        )))
    }
}

fn parse_doc_id(args: &Args, op: &str) -> Result<MemoryId, ToolError> {
    let id_str = args
        .doc_id
        .as_deref()
        .ok_or_else(|| ToolError::InvalidArgs(format!("doc_id required for op={op}")))?;
    MemoryId::from_str(id_str).map_err(|e| ToolError::InvalidArgs(format!("doc_id: {e}")))
}

fn doc_to_json(doc: &MemoryDoc) -> Value {
    json!({
        "id": doc.id.to_string(),
        "scope": doc.scope,
        "body": doc.body,
        "created_at": doc.created_at.to_rfc3339(),
        "updated_at": doc.updated_at.to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use crabgent_core::{MemoryId, Tool, ToolError};
    use crabgent_store::{MemoryDoc, MemoryStore};
    use serde_json::json;

    use crate::ops::test_support::{alice_ctx, alice_scope, alice_scope_value, allow_all_tool};

    #[tokio::test]
    async fn get_unknown_doc_returns_null_doc() {
        let (tool, _) = allow_all_tool();
        let unknown = MemoryId::new();
        let args = json!({
            "op": "get",
            "scope": alice_scope_value(),
            "doc_id": unknown.to_string()
        });
        let res = tool.execute(args, &alice_ctx()).await.expect("test result");
        assert!(res["doc"].is_null());
    }

    #[tokio::test]
    async fn delete_returns_false_for_unknown_id() {
        let (tool, _) = allow_all_tool();
        let unknown = MemoryId::new();
        let args = json!({
            "op": "delete",
            "scope": alice_scope_value(),
            "doc_id": unknown.to_string()
        });
        let res = tool.execute(args, &alice_ctx()).await.expect("test result");
        assert_eq!(res["deleted"], false);
    }

    #[tokio::test]
    async fn get_denies_document_outside_requested_scope() {
        let (tool, store) = allow_all_tool();
        let id = store
            .store(&MemoryDoc::new(alice_scope(), "secret"))
            .await
            .expect("test result");
        let args = json!({
            "op": "get",
            "scope": {"owner": "bob"},
            "doc_id": id.to_string()
        });
        let err = tool
            .execute(args, &alice_ctx())
            .await
            .expect_err("expected error");
        assert!(
            matches!(err, ToolError::Permission(msg) if msg.contains("outside requested scope"))
        );
    }

    #[tokio::test]
    async fn delete_denies_document_outside_requested_scope() {
        let (tool, store) = allow_all_tool();
        let id = store
            .store(&MemoryDoc::new(alice_scope(), "secret"))
            .await
            .expect("test result");
        let args = json!({
            "op": "delete",
            "scope": {"owner": "bob"},
            "doc_id": id.to_string()
        });
        let err = tool
            .execute(args, &alice_ctx())
            .await
            .expect_err("expected error");
        assert!(
            matches!(err, ToolError::Permission(msg) if msg.contains("outside requested scope"))
        );
        assert!(store.get(&id).await.expect("test result").is_some());
    }
}
