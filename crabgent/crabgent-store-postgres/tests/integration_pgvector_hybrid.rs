#[path = "../src/test_helpers.rs"]
mod test_helpers;

use chrono::{Duration, Utc};
use crabgent_core::{MemoryId, MemoryScope, Owner, SearchQuery};
use crabgent_store::{MemoryDoc, MemoryStore};
use crabgent_store_postgres::PostgresStore;
use test_helpers::postgres_test_ctx_with_embedding_dim;
use uuid::Uuid;

const EMBEDDING_DIM: usize = 8;

fn scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new(format!("pgvector-hybrid-{}", Uuid::now_v7())))
}

fn embedding(slot: usize) -> Vec<f32> {
    let mut vector = vec![0.0; EMBEDDING_DIM];
    if let Some(value) = vector.get_mut(slot) {
        *value = 1.0;
    }
    vector
}

fn doc(scope: MemoryScope, body: &str, slot: usize, age_hours: i64) -> MemoryDoc {
    let created_at = Utc::now() - Duration::hours(age_hours);
    let mut doc = MemoryDoc::new(scope, body);
    doc.embedding = Some(embedding(slot));
    doc.created_at = created_at;
    doc.updated_at = created_at;
    doc
}

async fn store_doc(store: &PostgresStore, doc: &MemoryDoc) -> MemoryId {
    store.memory_store().store(doc).await.expect("store memory")
}

#[tokio::test]
async fn pgvector_hybrid_search_uses_cosine_similarity() {
    let ctx = postgres_test_ctx_with_embedding_dim(EMBEDDING_DIM).await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = scope();

    let recorded_dim: i32 = sqlx::query_scalar(
        "SELECT dim FROM crabgent_vector_migrations WHERE name = 'add_pgvector'",
    )
    .fetch_one(&ctx.pool)
    .await
    .expect("read vector migration dimension");
    let expected_dim = i32::try_from(EMBEDDING_DIM).expect("test embedding dim fits i32");
    assert_eq!(recorded_dim, expected_dim);

    let old = store_doc(
        &store,
        &doc(scope.clone(), "anchor marker shared memory old", 0, 2),
    )
    .await;
    let target = store_doc(
        &store,
        &doc(scope.clone(), "anchor marker shared memory target", 1, 1),
    )
    .await;
    let newest = store_doc(
        &store,
        &doc(scope.clone(), "anchor marker shared memory newest", 2, 0),
    )
    .await;
    assert_ne!(old, target);

    let fts_hits = store
        .memory_store()
        .search(&SearchQuery::new("marker").scope(scope.clone()).limit(3))
        .await
        .expect("fts search");
    assert_eq!(fts_hits.first().map(|hit| &hit.id), Some(&newest));
    assert!(fts_hits.iter().all(|hit| hit.cosine_similarity.is_none()));

    let mut hybrid_query = SearchQuery::new("marker").scope(scope).limit(3);
    hybrid_query.embedding = Some(embedding(1));
    let hybrid_hits = store
        .memory_store()
        .search(&hybrid_query)
        .await
        .expect("hybrid search");

    assert_eq!(hybrid_hits.first().map(|hit| &hit.id), Some(&target));
    assert!(
        hybrid_hits
            .first()
            .and_then(|hit| hit.cosine_similarity)
            .is_some_and(|similarity| similarity > 0.99)
    );
}
