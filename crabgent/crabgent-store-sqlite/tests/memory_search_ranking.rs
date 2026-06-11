use chrono::{Duration, Utc};
use crabgent_core::{MemoryId, MemoryScope, Owner, SearchQuery};
use crabgent_store::{MemoryDoc, MemoryStore};
use crabgent_store_sqlite::SqliteStore;

async fn store() -> SqliteStore {
    SqliteStore::open_in_memory()
        .await
        .expect("open sqlite store")
}

fn scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new("sqlite-memory-search-ranking"))
}

fn doc(body: &str, importance: f32, created_at: chrono::DateTime<Utc>) -> MemoryDoc {
    let mut doc = MemoryDoc::new(scope(), body);
    doc.importance = Some(importance);
    doc.created_at = created_at;
    doc.updated_at = created_at;
    doc
}

async fn store_doc(store: &SqliteStore, doc: &MemoryDoc) -> MemoryId {
    store.memory().store(doc).await.expect("store memory")
}

async fn search(store: &SqliteStore, query: &str) -> Vec<crabgent_store::MemoryHit> {
    store
        .memory()
        .search(&SearchQuery::new(query).scope(scope()))
        .await
        .expect("search memory")
}

#[tokio::test]
async fn memory_search_sqlite_orders_by_importance_when_relevance_tied() {
    let store = store().await;
    let now = Utc::now();
    let low = doc("expert knowledge", 0.3, now);
    let high = doc("expert knowledge", 0.9, now - Duration::hours(1));
    let low_id = store_doc(&store, &low).await;
    let high_id = store_doc(&store, &high).await;

    let hits = search(&store, "expert").await;

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, high_id);
    assert_eq!(hits[1].id, low_id);
}

#[tokio::test]
async fn memory_search_sqlite_orders_by_recency_when_importance_tied() {
    let store = store().await;
    let now = Utc::now();
    let old = doc("expert knowledge", 0.5, now - Duration::hours(1));
    let new = doc("expert knowledge", 0.5, now);
    let old_id = store_doc(&store, &old).await;
    let new_id = store_doc(&store, &new).await;

    let hits = search(&store, "expert").await;

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, new_id);
    assert_eq!(hits[1].id, old_id);
}

#[tokio::test]
async fn memory_search_sqlite_orders_no_query_path_by_importance() {
    let store = store().await;
    let now = Utc::now();
    let low = doc("low priority", 0.3, now);
    let high = doc("high priority", 0.9, now - Duration::hours(1));
    let low_id = store_doc(&store, &low).await;
    let high_id = store_doc(&store, &high).await;

    let hits = search(&store, "").await;

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, high_id);
    assert_eq!(hits[1].id, low_id);
}

#[tokio::test]
async fn memory_search_sqlite_bm25_dominates_importance() {
    let store = store().await;
    let now = Utc::now();
    let better_relevance = doc("expert expert", 0.3, now - Duration::hours(1));
    let higher_importance = doc(
        "general topic with expert mention and other filler words for length",
        0.9,
        now,
    );
    let better_relevance_id = store_doc(&store, &better_relevance).await;
    let higher_importance_id = store_doc(&store, &higher_importance).await;

    let hits = search(&store, "expert").await;

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, better_relevance_id);
    assert_eq!(hits[1].id, higher_importance_id);
}
