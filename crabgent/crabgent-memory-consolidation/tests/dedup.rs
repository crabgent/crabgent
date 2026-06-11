mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::{
    EmbeddingError, EmbeddingProvider, EmbeddingRequest, EmbeddingResponse, MemoryId, MemoryScope,
    ModelId, RunCtx, SearchQuery,
};
use crabgent_memory::MemoryClass;
use crabgent_memory_consolidation::{
    ConflictDecision, ConsolidationError, Deduplicator, ExtractedFact,
};
use crabgent_store::{MemoryDoc, MemoryHit, MemoryMemoryStore, MemoryStore, StoreError};
use tokio_util::sync::CancellationToken;

use common::{StaticConflictResolver, fact, scope, semantic_doc, store_doc, token};

struct TestEmbeddingProvider {
    model: ModelId,
    result: Result<Vec<f32>, EmbeddingError>,
}

impl TestEmbeddingProvider {
    fn fixed(vector: Vec<f32>) -> Self {
        Self {
            model: ModelId::new("test-embedding"),
            result: Ok(vector),
        }
    }

    fn failing(err: EmbeddingError) -> Self {
        Self {
            model: ModelId::new("test-embedding"),
            result: Err(err),
        }
    }
}

#[async_trait]
impl EmbeddingProvider for TestEmbeddingProvider {
    fn dim(&self) -> usize {
        self.result.as_ref().map_or(3, Vec::len)
    }

    fn model_id(&self) -> &ModelId {
        &self.model
    }

    async fn embed(
        &self,
        req: EmbeddingRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<EmbeddingResponse, EmbeddingError> {
        let vector = self.result.clone()?;
        Ok(EmbeddingResponse {
            vectors: req.texts.iter().map(|_| vector.clone()).collect(),
            model: req.model.unwrap_or_else(|| self.model.clone()),
            dim: vector.len(),
            usage: None,
        })
    }
}

struct RecordingStore {
    inner: MemoryMemoryStore,
    last_query: Mutex<Option<SearchQuery>>,
}

impl Default for RecordingStore {
    fn default() -> Self {
        Self {
            inner: MemoryMemoryStore::default(),
            last_query: Mutex::new(None),
        }
    }
}

impl RecordingStore {
    fn last_query(&self) -> SearchQuery {
        self.last_query
            .lock()
            .expect("mutex should not be poisoned")
            .clone()
            .expect("search should be recorded")
    }
}

#[async_trait]
impl MemoryStore for RecordingStore {
    async fn search(&self, query: &SearchQuery) -> Result<Vec<MemoryHit>, StoreError> {
        *self
            .last_query
            .lock()
            .expect("mutex should not be poisoned") = Some(query.clone());
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
}

#[tokio::test]
async fn dedup_high_similarity_upserts_existing() {
    let store = Arc::new(MemoryMemoryStore::default());
    let mut existing = semantic_doc("alpha beta gamma delta");
    store_doc(&store, &existing).await;
    let dedup = Deduplicator::new(store.clone());

    let result = dedup
        .dedup(
            &fact("alpha beta gamma delta"),
            &scope(),
            &StaticConflictResolver::new(ConflictDecision::Skip),
            &token(),
        )
        .await
        .expect("test result");
    existing = store
        .get(&existing.id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert!(result.updated);
    assert_eq!(existing.body, "alpha beta gamma delta");
}

#[tokio::test]
async fn dedup_low_similarity_creates_new() {
    let store = Arc::new(MemoryMemoryStore::default());
    store_doc(&store, &semantic_doc("unrelated old memory")).await;
    let dedup = Deduplicator::new(store.clone());

    let result = dedup
        .dedup(
            &fact("brand new durable semantic fact"),
            &scope(),
            &StaticConflictResolver::new(ConflictDecision::Skip),
            &token(),
        )
        .await
        .expect("test result");

    assert!(result.created);
}

#[tokio::test]
async fn dedup_create_stores_embedding_when_provider_configured() {
    let store = Arc::new(MemoryMemoryStore::default());
    let dedup = Deduplicator::new(store.clone())
        .with_embedding_provider(Arc::new(TestEmbeddingProvider::fixed(vec![0.25, 0.5, 1.0])));

    let result = dedup
        .dedup(
            &fact("brand new durable semantic fact"),
            &scope(),
            &StaticConflictResolver::new(ConflictDecision::Skip),
            &token(),
        )
        .await
        .expect("test result");
    let id = result.memory_id.expect("created memory id");
    let doc = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert!(result.created);
    assert_eq!(doc.embedding, Some(vec![0.25, 0.5, 1.0]));
}

#[tokio::test]
async fn dedup_update_replaces_embedding_when_provider_configured() {
    let store = Arc::new(MemoryMemoryStore::default());
    let mut existing = semantic_doc("alpha beta gamma delta");
    existing.embedding = Some(vec![1.0, 0.0, 0.0]);
    store_doc(&store, &existing).await;
    let dedup = Deduplicator::new(store.clone())
        .with_embedding_provider(Arc::new(TestEmbeddingProvider::fixed(vec![0.0, 1.0, 0.0])));

    let result = dedup
        .dedup(
            &fact("alpha beta gamma delta"),
            &scope(),
            &StaticConflictResolver::new(ConflictDecision::Skip),
            &token(),
        )
        .await
        .expect("test result");
    let doc = store
        .get(&existing.id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert!(result.updated);
    assert_eq!(doc.embedding, Some(vec![0.0, 1.0, 0.0]));
}

#[tokio::test]
async fn dedup_embedding_failure_stores_without_embedding() {
    let store = Arc::new(MemoryMemoryStore::default());
    let dedup = Deduplicator::new(store.clone()).with_embedding_provider(Arc::new(
        TestEmbeddingProvider::failing(EmbeddingError::Other("boom".to_owned())),
    ));

    let result = dedup
        .dedup(
            &fact("brand new durable semantic fact"),
            &scope(),
            &StaticConflictResolver::new(ConflictDecision::Skip),
            &token(),
        )
        .await
        .expect("test result");
    let id = result.memory_id.expect("created memory id");
    let doc = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert!(result.created);
    assert_eq!(doc.embedding, None);
}

#[tokio::test]
async fn dedup_embedding_cancel_aborts_run() {
    let store = Arc::new(MemoryMemoryStore::default());
    let dedup = Deduplicator::new(store)
        .with_embedding_provider(Arc::new(TestEmbeddingProvider::fixed(vec![0.25, 0.5, 1.0])));
    let token = token();
    token.cancel();

    let err = dedup
        .dedup(
            &fact("brand new durable semantic fact"),
            &scope(),
            &StaticConflictResolver::new(ConflictDecision::Skip),
            &token,
        )
        .await
        .expect_err("cancelled embedding should abort");

    assert!(matches!(err, ConsolidationError::Cancelled));
}

#[tokio::test]
async fn dedup_search_uses_query_embedding_when_provider_configured() {
    let store = Arc::new(RecordingStore::default());
    store
        .store(&semantic_doc("unrelated old memory"))
        .await
        .expect("store doc");
    let dedup = Deduplicator::new(store.clone())
        .with_embedding_provider(Arc::new(TestEmbeddingProvider::fixed(vec![0.25, 0.5, 1.0])));

    dedup
        .dedup(
            &fact("brand new durable semantic fact"),
            &scope(),
            &StaticConflictResolver::new(ConflictDecision::Skip),
            &token(),
        )
        .await
        .expect("test result");

    assert_eq!(store.last_query().embedding, Some(vec![0.25, 0.5, 1.0]));
}

#[tokio::test]
async fn dedup_search_filters_by_fact_kind() {
    let store = Arc::new(MemoryMemoryStore::default());
    let mut existing = semantic_doc("alpha beta gamma delta");
    existing.class = Some(MemoryClass::Skill.as_str().to_owned());
    store_doc(&store, &existing).await;
    let dedup = Deduplicator::new(store.clone());
    let fact = ExtractedFact::new(
        "alpha beta gamma delta",
        MemoryClass::Skill.as_str(),
        0.6,
        1.0,
    );

    let result = dedup
        .dedup(
            &fact,
            &scope(),
            &StaticConflictResolver::new(ConflictDecision::Skip),
            &token(),
        )
        .await
        .expect("test result");

    assert!(result.updated);
    assert_eq!(result.memory_id, Some(existing.id));
}

#[tokio::test]
async fn dedup_zone_triggers_conflict_resolver() {
    let store = Arc::new(MemoryMemoryStore::default());
    store_doc(&store, &semantic_doc("alpha beta gamma delta epsilon")).await;
    let dedup = Deduplicator::new(store);

    let result = dedup
        .dedup(
            &fact("alpha beta gamma delta"),
            &scope(),
            &StaticConflictResolver::new(ConflictDecision::KeepExisting),
            &token(),
        )
        .await
        .expect("test result");

    assert!(result.conflict);
    assert_eq!(result.decision, Some(ConflictDecision::KeepExisting));
}
