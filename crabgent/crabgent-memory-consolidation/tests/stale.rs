mod common;

use std::sync::Arc;

use chrono::{Duration, Utc};
use crabgent_memory::MemoryClass;
use crabgent_memory_consolidation::{StaleCleaner, StalePolicy};
use crabgent_store::{MemoryMemoryStore, MemoryStore};

use common::{episodic_doc, scope, semantic_doc, store_doc};

fn cleaner(store: Arc<MemoryMemoryStore>) -> StaleCleaner {
    StaleCleaner::new(store, StalePolicy::default())
}

#[tokio::test]
async fn stale_cleanup_archives_old_low_importance_episodic() {
    let store = Arc::new(MemoryMemoryStore::default());
    let mut doc = episodic_doc("old low importance episodic");
    doc.importance = Some(0.1);
    doc.updated_at = Utc::now() - Duration::days(31);
    let id = doc.id.clone();
    store_doc(&store, &doc).await;

    let archived = cleaner(store.clone())
        .clean(&scope())
        .await
        .expect("test result");
    let stored = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(archived, 1);
    assert!(stored.archived_at.is_some());
}

#[tokio::test]
async fn stale_cleanup_skips_semantic() {
    let store = Arc::new(MemoryMemoryStore::default());
    let mut doc = semantic_doc("semantic memories are never auto archived");
    doc.importance = Some(0.1);
    doc.updated_at = Utc::now() - Duration::days(31);
    let id = doc.id.clone();
    store_doc(&store, &doc).await;

    let archived = cleaner(store.clone())
        .clean(&scope())
        .await
        .expect("test result");
    let stored = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(doc.class.as_deref(), Some(MemoryClass::Semantic.as_str()));
    assert_eq!(archived, 0);
    assert!(stored.archived_at.is_none());
}

#[tokio::test]
async fn stale_cleanup_skips_high_importance_episodic() {
    let store = Arc::new(MemoryMemoryStore::default());
    let mut doc = episodic_doc("old high importance episodic");
    doc.importance = Some(0.9);
    doc.updated_at = Utc::now() - Duration::days(31);
    let id = doc.id.clone();
    store_doc(&store, &doc).await;

    let archived = cleaner(store.clone())
        .clean(&scope())
        .await
        .expect("test result");
    let stored = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(archived, 0);
    assert!(stored.archived_at.is_none());
}
