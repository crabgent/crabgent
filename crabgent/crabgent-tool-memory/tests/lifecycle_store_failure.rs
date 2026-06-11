//! Lifecycle tool error-path coverage.
//!
//! Each test runs a lifecycle tool against a `FailingMemoryStore` that
//! returns `StoreError` carrying internal detail. The assertion proves
//! the LLM-visible `ToolError::Execution` message is opaque (only the
//! op label + the "backend unavailable" marker), so `StoreError::Display`
//! never reaches the LLM. Mirrors `crabgent-tool-task` integration tests
//! that verify the matching `store_unavailable` helper there.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::{MemoryId, MemoryScope, Owner, SearchQuery, Subject, Tool, ToolCtx, ToolError};
use crabgent_store::{MemoryDoc, MemoryHit, MemoryStore, StoreError};
use crabgent_tool_memory::{ArchiveTool, ExtendExpiryTool, ForgetTool, UnarchiveTool};
use serde_json::json;

struct FailingMemoryStore;

#[async_trait]
impl MemoryStore for FailingMemoryStore {
    async fn search(&self, _q: &SearchQuery) -> Result<Vec<MemoryHit>, StoreError> {
        Ok(vec![])
    }
    async fn store(&self, _doc: &MemoryDoc) -> Result<MemoryId, StoreError> {
        Err(StoreError::Conflict(
            "internal: backend-token=secret".into(),
        ))
    }
    async fn get(&self, _id: &MemoryId) -> Result<Option<MemoryDoc>, StoreError> {
        // load_in_scope expects Some(doc) so the follow-up call hits the
        // intended error path. Stub doc carries the alice scope so the
        // scope check passes.
        Ok(Some(MemoryDoc::new(
            MemoryScope::for_owner(Owner::new("alice")),
            "stub",
        )))
    }
    async fn delete(&self, _id: &MemoryId) -> Result<bool, StoreError> {
        Ok(false)
    }
    async fn delete_scoped(
        &self,
        _id: &MemoryId,
        _scope: &MemoryScope,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Conflict(
            "internal: backend-token=secret".into(),
        ))
    }
    async fn archive(&self, _id: &MemoryId, _at: DateTime<Utc>) -> Result<bool, StoreError> {
        Err(StoreError::Conflict(
            "internal: backend-token=secret".into(),
        ))
    }
    async fn unarchive(&self, _id: &MemoryId) -> Result<bool, StoreError> {
        Err(StoreError::Conflict(
            "internal: backend-token=secret".into(),
        ))
    }
    async fn extend_expiry(
        &self,
        _id: &MemoryId,
        _new_expiry: Option<DateTime<Utc>>,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Conflict(
            "internal: backend-token=secret".into(),
        ))
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

fn assert_opaque(err: ToolError, op_prefix: &str) {
    match err {
        ToolError::Execution(msg) => {
            assert!(
                msg.starts_with(op_prefix),
                "expected prefix {op_prefix:?}, got {msg:?}"
            );
            assert!(
                msg.contains("backend unavailable"),
                "expected opaque marker, got {msg:?}"
            );
            assert!(
                !msg.contains("backend-token=secret"),
                "StoreError Display must NOT leak: {msg:?}"
            );
        }
        other => {
            assert!(
                matches!(other, ToolError::Execution(_)),
                "expected Execution, got {other:?}"
            );
        }
    }
}

fn alice_ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("alice"))
}

fn alice_scope_value() -> serde_json::Value {
    json!({ "owner": "alice" })
}

fn failing() -> Arc<dyn MemoryStore> {
    Arc::new(FailingMemoryStore)
}

#[tokio::test]
async fn archive_store_failure_returns_opaque_execution() {
    let tool = ArchiveTool::new(failing(), Arc::new(AllowAllPolicy));
    let id = MemoryId::new();
    let err = tool
        .execute(
            json!({ "scope": alice_scope_value(), "doc_id": id.to_string() }),
            &alice_ctx(),
        )
        .await
        .expect_err("store failure must surface");
    assert_opaque(err, "memory_archive:");
}

#[tokio::test]
async fn unarchive_store_failure_returns_opaque_execution() {
    let tool = UnarchiveTool::new(failing(), Arc::new(AllowAllPolicy));
    let id = MemoryId::new();
    let err = tool
        .execute(
            json!({ "scope": alice_scope_value(), "doc_id": id.to_string() }),
            &alice_ctx(),
        )
        .await
        .expect_err("store failure must surface");
    assert_opaque(err, "memory_unarchive:");
}

#[tokio::test]
async fn extend_expiry_store_failure_returns_opaque_execution() {
    let tool = ExtendExpiryTool::new(failing(), Arc::new(AllowAllPolicy));
    let id = MemoryId::new();
    let err = tool
        .execute(
            json!({
                "scope": alice_scope_value(),
                "doc_id": id.to_string(),
                "expires_at": Utc::now().to_rfc3339()
            }),
            &alice_ctx(),
        )
        .await
        .expect_err("store failure must surface");
    assert_opaque(err, "memory_extend_expiry:");
}

#[tokio::test]
async fn forget_store_failure_returns_opaque_execution() {
    let tool = ForgetTool::new(failing(), Arc::new(AllowAllPolicy));
    let id = MemoryId::new();
    let err = tool
        .execute(
            json!({ "scope": alice_scope_value(), "doc_id": id.to_string() }),
            &alice_ctx(),
        )
        .await
        .expect_err("store failure must surface");
    assert_opaque(err, "memory_forget:");
}

/// Store stub for the scope-mismatch path: `get` returns a doc whose
/// scope owner is "alice", but the caller requests scope owner "bob".
/// `load_in_scope` should return `ToolError::Permission` and the
/// consumer archive/unarchive/extend/delete call must NOT fire.
struct ScopeMismatchStore;

#[async_trait]
impl MemoryStore for ScopeMismatchStore {
    async fn search(&self, _q: &SearchQuery) -> Result<Vec<MemoryHit>, StoreError> {
        Ok(vec![])
    }
    async fn store(&self, _doc: &MemoryDoc) -> Result<MemoryId, StoreError> {
        Ok(MemoryId::new())
    }
    async fn get(&self, _id: &MemoryId) -> Result<Option<MemoryDoc>, StoreError> {
        Ok(Some(MemoryDoc::new(
            MemoryScope::for_owner(Owner::new("alice")),
            "out-of-bob-scope",
        )))
    }
    async fn delete(&self, _id: &MemoryId) -> Result<bool, StoreError> {
        Err(StoreError::Backend(
            "delete must NOT run after scope mismatch".to_owned(),
        ))
    }
    async fn delete_scoped(
        &self,
        _id: &MemoryId,
        _scope: &MemoryScope,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Backend(
            "delete_scoped must NOT run after scope mismatch".to_owned(),
        ))
    }
    async fn archive(&self, _id: &MemoryId, _at: DateTime<Utc>) -> Result<bool, StoreError> {
        Err(StoreError::Backend(
            "archive must NOT run after scope mismatch".to_owned(),
        ))
    }
    async fn unarchive(&self, _id: &MemoryId) -> Result<bool, StoreError> {
        Err(StoreError::Backend(
            "unarchive must NOT run after scope mismatch".to_owned(),
        ))
    }
    async fn extend_expiry(
        &self,
        _id: &MemoryId,
        _new_expiry: Option<DateTime<Utc>>,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Backend(
            "extend_expiry must NOT run after scope mismatch".to_owned(),
        ))
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

fn assert_scope_mismatch(err: ToolError, op_marker: &str) {
    match err {
        ToolError::Permission(msg) => {
            assert!(
                msg.contains("outside requested scope"),
                "expected scope mismatch, got {msg:?}"
            );
            assert!(
                msg.contains(op_marker),
                "expected op marker {op_marker:?}, got {msg:?}"
            );
        }
        other => {
            assert!(
                matches!(other, ToolError::Permission(_)),
                "expected Permission, got {other:?}"
            );
        }
    }
}

fn mismatch_store() -> Arc<dyn MemoryStore> {
    Arc::new(ScopeMismatchStore)
}

#[tokio::test]
async fn archive_scope_mismatch_returns_permission_not_store_call() {
    let tool = ArchiveTool::new(mismatch_store(), Arc::new(AllowAllPolicy));
    let id = MemoryId::new();
    let err = tool
        .execute(
            json!({ "scope": { "owner": "bob" }, "doc_id": id.to_string() }),
            &alice_ctx(),
        )
        .await
        .expect_err("scope mismatch");
    assert_scope_mismatch(err, "memory_archive");
}

#[tokio::test]
async fn forget_scope_mismatch_returns_permission_not_store_call() {
    let tool = ForgetTool::new(mismatch_store(), Arc::new(AllowAllPolicy));
    let id = MemoryId::new();
    let err = tool
        .execute(
            json!({ "scope": { "owner": "bob" }, "doc_id": id.to_string() }),
            &alice_ctx(),
        )
        .await
        .expect_err("scope mismatch");
    assert_scope_mismatch(err, "memory_forget");
}
