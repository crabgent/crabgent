//! Separate lifecycle tools for memory documents.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::error::ToolError;
use crabgent_core::tool::{Tool, ToolCtx, gate_tool_action, parse_args_with_context};
use crabgent_core::{Action, MemoryId, MemoryScope, PolicyHook};
use crabgent_store::{MemoryDoc, MemoryStore};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::store_unavailable;

#[derive(Clone)]
struct LifecycleState {
    store: Arc<dyn MemoryStore>,
    policy: Arc<dyn PolicyHook>,
}

impl LifecycleState {
    fn new(store: Arc<dyn MemoryStore>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { store, policy }
    }

    async fn gate(&self, action: &Action, ctx: &ToolCtx) -> Result<(), ToolError> {
        gate_tool_action(self.policy.as_ref(), ctx, action).await
    }

    async fn load_in_scope(
        &self,
        id: &MemoryId,
        scope: &MemoryScope,
        op: &str,
    ) -> Result<Option<MemoryDoc>, ToolError> {
        let doc = self
            .store
            .get(id)
            .await
            .map_err(|err| store_unavailable(&format!("{op}.get"), &err))?;
        if let Some(doc) = &doc
            && !scope.matches(&doc.scope)
        {
            return Err(ToolError::Permission(format!(
                "{op}: document outside requested scope"
            )));
        }
        Ok(doc)
    }
}

#[derive(Debug, Deserialize)]
struct IdScopeArgs {
    scope: MemoryScope,
    doc_id: String,
}

impl IdScopeArgs {
    fn id(&self) -> Result<MemoryId, ToolError> {
        MemoryId::from_str(&self.doc_id)
            .map_err(|err| ToolError::InvalidArgs(format!("doc_id: {err}")))
    }
}

#[derive(Debug, Deserialize)]
struct ExtendExpiryArgs {
    scope: MemoryScope,
    doc_id: String,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

impl ExtendExpiryArgs {
    fn id(&self) -> Result<MemoryId, ToolError> {
        MemoryId::from_str(&self.doc_id)
            .map_err(|err| ToolError::InvalidArgs(format!("doc_id: {err}")))
    }
}

/// Define a lifecycle tool wrapping shared [`LifecycleState`]. The `Tool` impl
/// (with its distinct op/action logic) is written separately per tool.
macro_rules! lifecycle_tool {
    ($name:ident) => {
        pub struct $name {
            state: LifecycleState,
        }

        impl $name {
            pub fn new(store: Arc<dyn MemoryStore>, policy: Arc<dyn PolicyHook>) -> Self {
                Self {
                    state: LifecycleState::new(store, policy),
                }
            }
        }
    };
}

lifecycle_tool!(ArchiveTool);
lifecycle_tool!(UnarchiveTool);
lifecycle_tool!(ExtendExpiryTool);
lifecycle_tool!(ForgetTool);

#[async_trait]
impl Tool for ArchiveTool {
    fn name(&self) -> &'static str {
        "memory_archive"
    }

    fn description(&self) -> &'static str {
        "Archive one memory document by doc_id and scope. Archived records are hidden from default memory search unless include_archived is true."
    }

    fn parameters_schema(&self) -> Value {
        id_scope_schema("Archive memory document id.")
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: IdScopeArgs = parse_args_with_context(args, "memory_archive args")?;
        let id = args.id()?;
        let action = Action::MemoryArchive {
            id: id.clone(),
            scope: args.scope.clone(),
        };
        self.state.gate(&action, ctx).await?;
        if self
            .state
            .load_in_scope(&id, &args.scope, "memory_archive")
            .await?
            .is_none()
        {
            return Ok(json!({ "archived": false }));
        }
        let archived = self
            .state
            .store
            .archive(&id, Utc::now())
            .await
            .map_err(|err| store_unavailable("memory_archive", &err))?;
        Ok(json!({ "archived": archived }))
    }
}

#[async_trait]
impl Tool for UnarchiveTool {
    fn name(&self) -> &'static str {
        "memory_unarchive"
    }

    fn description(&self) -> &'static str {
        "Remove the archive marker from one memory document by doc_id and scope."
    }

    fn parameters_schema(&self) -> Value {
        id_scope_schema("Unarchive memory document id.")
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: IdScopeArgs = parse_args_with_context(args, "memory_unarchive args")?;
        let id = args.id()?;
        let action = Action::MemoryUnarchive {
            id: id.clone(),
            scope: args.scope.clone(),
        };
        self.state.gate(&action, ctx).await?;
        if self
            .state
            .load_in_scope(&id, &args.scope, "memory_unarchive")
            .await?
            .is_none()
        {
            return Ok(json!({ "unarchived": false }));
        }
        let unarchived = self
            .state
            .store
            .unarchive(&id)
            .await
            .map_err(|err| store_unavailable("memory_unarchive", &err))?;
        Ok(json!({ "unarchived": unarchived }))
    }
}

#[async_trait]
impl Tool for ExtendExpiryTool {
    fn name(&self) -> &'static str {
        "memory_extend_expiry"
    }

    fn description(&self) -> &'static str {
        "Set or clear expires_at for one memory document by doc_id and scope. Pass expires_at as an RFC3339 date-time string or null to clear expiry."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["scope", "doc_id"],
            "properties": {
                "scope": crabgent_core::tool::memory_scope_schema(),
                "doc_id": {"type": "string", "description": "Memory document id."},
                "expires_at": {"type": ["string", "null"], "format": "date-time"}
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: ExtendExpiryArgs = parse_args_with_context(args, "memory_extend_expiry args")?;
        let id = args.id()?;
        let action = Action::MemoryExtendExpiry {
            id: id.clone(),
            scope: args.scope.clone(),
        };
        self.state.gate(&action, ctx).await?;
        if self
            .state
            .load_in_scope(&id, &args.scope, "memory_extend_expiry")
            .await?
            .is_none()
        {
            return Ok(json!({ "extended": false, "expires_at": null }));
        }
        let extended = self
            .state
            .store
            .extend_expiry(&id, args.expires_at)
            .await
            .map_err(|err| store_unavailable("memory_extend_expiry", &err))?;
        Ok(json!({
            "extended": extended,
            "expires_at": args.expires_at.map(|expires_at| expires_at.to_rfc3339())
        }))
    }
}

#[async_trait]
impl Tool for ForgetTool {
    fn name(&self) -> &'static str {
        "memory_forget"
    }

    fn description(&self) -> &'static str {
        "Hard-delete one memory document by doc_id and scope."
    }

    fn parameters_schema(&self) -> Value {
        id_scope_schema("Forget memory document id.")
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: IdScopeArgs = parse_args_with_context(args, "memory_forget args")?;
        let id = args.id()?;
        let action = Action::MemoryDelete {
            id: id.clone(),
            scope: args.scope.clone(),
        };
        self.state.gate(&action, ctx).await?;
        if self
            .state
            .load_in_scope(&id, &args.scope, "memory_forget")
            .await?
            .is_none()
        {
            return Ok(json!({ "deleted": false }));
        }
        let deleted = self
            .state
            .store
            .delete_scoped(&id, &args.scope)
            .await
            .map_err(|err| store_unavailable("memory_forget", &err))?;
        Ok(json!({ "deleted": deleted }))
    }
}

fn id_scope_schema(doc_description: &str) -> Value {
    json!({
        "type": "object",
        "required": ["scope", "doc_id"],
        "properties": {
            "scope": crabgent_core::tool::memory_scope_schema(),
            "doc_id": {"type": "string", "description": doc_description}
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use crabgent_core::{AllowAllPolicy, SearchQuery, Tool};
    use crabgent_store::{MemoryDoc, MemoryMemoryStore, MemoryStore};
    use serde_json::json;

    use crate::ops::test_support::{alice_ctx, alice_scope, alice_scope_value};
    use crate::{ArchiveTool, ExtendExpiryTool, ForgetTool, MemoryTool, UnarchiveTool};

    fn shared_store() -> Arc<MemoryMemoryStore> {
        Arc::new(MemoryMemoryStore::default())
    }

    fn archive_tool(store: Arc<MemoryMemoryStore>) -> ArchiveTool {
        let store_dyn: Arc<dyn MemoryStore> = store;
        ArchiveTool::new(store_dyn, Arc::new(AllowAllPolicy))
    }

    fn unarchive_tool(store: Arc<MemoryMemoryStore>) -> UnarchiveTool {
        let store_dyn: Arc<dyn MemoryStore> = store;
        UnarchiveTool::new(store_dyn, Arc::new(AllowAllPolicy))
    }

    fn extend_tool(store: Arc<MemoryMemoryStore>) -> ExtendExpiryTool {
        let store_dyn: Arc<dyn MemoryStore> = store;
        ExtendExpiryTool::new(store_dyn, Arc::new(AllowAllPolicy))
    }

    fn forget_tool(store: Arc<MemoryMemoryStore>) -> ForgetTool {
        let store_dyn: Arc<dyn MemoryStore> = store;
        ForgetTool::new(store_dyn, Arc::new(AllowAllPolicy))
    }

    fn memory_tool(store: Arc<MemoryMemoryStore>) -> MemoryTool {
        let store_dyn: Arc<dyn MemoryStore> = store;
        MemoryTool::new(store_dyn, Arc::new(AllowAllPolicy), None)
    }

    async fn store_doc(store: &MemoryMemoryStore, body: &str) -> crabgent_core::MemoryId {
        store
            .store(&MemoryDoc::new(alice_scope(), body))
            .await
            .expect("test result")
    }

    fn id_args(id: &crabgent_core::MemoryId) -> serde_json::Value {
        json!({"scope": alice_scope_value(), "doc_id": id.to_string()})
    }

    async fn default_search_count(store: &MemoryMemoryStore, query: &str) -> usize {
        store
            .search(&SearchQuery::new(query).scope(alice_scope()))
            .await
            .expect("test result")
            .len()
    }

    #[tokio::test]
    async fn archive_tool_hides_from_default_search() {
        let store = shared_store();
        let id = store_doc(&store, "archivable memory").await;

        archive_tool(store.clone())
            .execute(id_args(&id), &alice_ctx())
            .await
            .expect("test result");

        assert_eq!(default_search_count(&store, "archivable").await, 0);
    }

    #[tokio::test]
    async fn archive_tool_visible_with_include_archived() {
        let store = shared_store();
        let id = store_doc(&store, "archivable memory").await;
        archive_tool(store.clone())
            .execute(id_args(&id), &alice_ctx())
            .await
            .expect("test result");

        let result = memory_tool(store)
            .execute(
                json!({
                    "op": "search",
                    "scope": alice_scope_value(),
                    "query": "archivable",
                    "include_archived": true
                }),
                &alice_ctx(),
            )
            .await
            .expect("test result");

        assert_eq!(result["count"], 1);
    }

    #[tokio::test]
    async fn unarchive_tool_restores() {
        let store = shared_store();
        let id = store_doc(&store, "restorable memory").await;
        archive_tool(store.clone())
            .execute(id_args(&id), &alice_ctx())
            .await
            .expect("test result");

        unarchive_tool(store.clone())
            .execute(id_args(&id), &alice_ctx())
            .await
            .expect("test result");

        assert_eq!(default_search_count(&store, "restorable").await, 1);
    }

    #[tokio::test]
    async fn extend_expiry_tool_postpones() {
        let store = shared_store();
        let mut doc = MemoryDoc::new(alice_scope(), "temporary memory");
        doc.expires_at = Some(Utc::now() - chrono::Duration::hours(1));
        let id = store.store(&doc).await.expect("test result");
        assert_eq!(default_search_count(&store, "temporary").await, 0);

        extend_tool(store.clone())
            .execute(
                json!({
                    "scope": alice_scope_value(),
                    "doc_id": id.to_string(),
                    "expires_at": (Utc::now() + chrono::Duration::hours(1)).to_rfc3339()
                }),
                &alice_ctx(),
            )
            .await
            .expect("test result");

        assert_eq!(default_search_count(&store, "temporary").await, 1);
    }

    #[tokio::test]
    async fn forget_tool_hard_deletes() {
        let store = shared_store();
        let id = store_doc(&store, "forgettable memory").await;

        forget_tool(store.clone())
            .execute(id_args(&id), &alice_ctx())
            .await
            .expect("test result");

        assert!(store.get(&id).await.expect("test result").is_none());
    }
}
