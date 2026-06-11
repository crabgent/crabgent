mod common;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::{MemoryId, MemoryScope, SearchQuery};
use crabgent_memory::MemoryClass;
use crabgent_memory_consolidation::{
    ConflictDecision, ConsolidationConfig, ConsolidationRunner, Deduplicator, StaleCleaner,
};
use crabgent_store::{
    MemoryDoc, MemoryHit, MemoryMemoryStore, MemoryRelation, MemoryStore, RelationId, RelationType,
    StoreError,
};

use common::{
    StaticConflictResolver, StaticFactExtractor, episodic_doc, fact, long_body, scope, store_doc,
    subject, token,
};

/// Forwards every `MemoryStore` method to an inner in-memory store, except
/// `relation_store`, which fails with a backend error. Used to prove relation
/// emission is fail-open: a relation write failure must not abort the run.
struct RelationFailStore {
    inner: Arc<MemoryMemoryStore>,
}

#[async_trait]
impl MemoryStore for RelationFailStore {
    async fn search(&self, query: &SearchQuery) -> Result<Vec<MemoryHit>, StoreError> {
        self.inner.search(query).await
    }

    async fn store(&self, doc: &MemoryDoc) -> Result<MemoryId, StoreError> {
        self.inner.store(doc).await
    }

    async fn get(&self, id: &MemoryId) -> Result<Option<MemoryDoc>, StoreError> {
        self.inner.get(id).await
    }

    async fn delete(&self, id: &MemoryId) -> Result<bool, StoreError> {
        self.inner.delete(id).await
    }

    async fn delete_scoped(&self, id: &MemoryId, scope: &MemoryScope) -> Result<bool, StoreError> {
        self.inner.delete_scoped(id, scope).await
    }

    async fn archive(&self, id: &MemoryId, at: DateTime<Utc>) -> Result<bool, StoreError> {
        self.inner.archive(id, at).await
    }

    async fn unarchive(&self, id: &MemoryId) -> Result<bool, StoreError> {
        self.inner.unarchive(id).await
    }

    async fn extend_expiry(
        &self,
        id: &MemoryId,
        new_expiry: Option<DateTime<Utc>>,
    ) -> Result<bool, StoreError> {
        self.inner.extend_expiry(id, new_expiry).await
    }

    async fn update_body(&self, id: &MemoryId, new_body: String) -> Result<bool, StoreError> {
        self.inner.update_body(id, new_body).await
    }

    async fn update_body_with_embedding(
        &self,
        id: &MemoryId,
        new_body: String,
        embedding: Option<Vec<f32>>,
    ) -> Result<bool, StoreError> {
        self.inner
            .update_body_with_embedding(id, new_body, embedding)
            .await
    }

    async fn relation_store(&self, _relation: &MemoryRelation) -> Result<RelationId, StoreError> {
        Err(StoreError::Backend("relation backend down".to_owned()))
    }

    async fn relation_delete(
        &self,
        from_id: &MemoryId,
        to_id: &MemoryId,
        relation_type: &RelationType,
        scope: &MemoryScope,
    ) -> Result<bool, StoreError> {
        self.inner
            .relation_delete(from_id, to_id, relation_type, scope)
            .await
    }

    async fn relation_neighbors(
        &self,
        ids: &[MemoryId],
        scope: &MemoryScope,
    ) -> Result<Vec<MemoryRelation>, StoreError> {
        self.inner.relation_neighbors(ids, scope).await
    }
}

#[tokio::test]
async fn relation_store_failure_does_not_abort_run() {
    let inner = Arc::new(MemoryMemoryStore::default());
    store_doc(&inner, &episodic_doc(long_body("Alice prefers tea"))).await;
    let store: Arc<dyn MemoryStore> = Arc::new(RelationFailStore {
        inner: inner.clone(),
    });
    let config = ConsolidationConfig::default();
    let runner = ConsolidationRunner::new(
        store.clone(),
        Arc::new(StaticFactExtractor::new(vec![fact(
            "Alice prefers tea in the afternoon",
        )])),
        Deduplicator::new(store.clone()),
        Arc::new(StaticConflictResolver::new(ConflictDecision::Skip)),
        StaleCleaner::new(store.clone(), config.stale_policy.clone()),
        Arc::new(crabgent_core::AllowAllPolicy),
        config,
    );

    let result = runner
        .run(&subject(), scope(), token())
        .await
        .expect("run still ok despite relation failure");

    assert_eq!(result.memories_created, 1);
    let semantic = inner
        .search(
            &SearchQuery::new("Alice prefers tea")
                .scope(scope())
                .class(MemoryClass::Semantic.as_str()),
        )
        .await
        .expect("semantic search");
    assert_eq!(semantic.len(), 1);
}
