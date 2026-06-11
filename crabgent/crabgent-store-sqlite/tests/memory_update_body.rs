use crabgent_core::{MemoryId, MemoryScope, Owner};
use crabgent_store::{MemoryDoc, MemoryStore};
use crabgent_store_sqlite::{DEFAULT_EMBEDDING_DIM, SqliteStore};

async fn store() -> SqliteStore {
    SqliteStore::open_in_memory().await.expect("open store")
}

fn alice_scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new("sqlite-memory-update-alice"))
}

#[tokio::test]
async fn update_body_sqlite_roundtrip() {
    let store = store().await;
    let id = store
        .memory()
        .store(&MemoryDoc::new(alice_scope(), "old body"))
        .await
        .expect("test result");

    assert!(
        store
            .memory()
            .update_body(&id, "new body keyword".to_owned())
            .await
            .expect("test result")
    );
    let updated = store
        .memory()
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(updated.body, "new body keyword");
    let hits = store
        .memory()
        .search(&crabgent_core::SearchQuery::new("keyword").scope(alice_scope()))
        .await
        .expect("test result");
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn update_body_sqlite_not_found_returns_false() {
    let store = store().await;

    assert!(
        !store
            .memory()
            .update_body(&MemoryId::new(), "new body".to_owned())
            .await
            .expect("test result")
    );
}

#[tokio::test]
async fn update_body_sqlite_clears_embedding() {
    let store = store().await;
    let mut doc = MemoryDoc::new(alice_scope(), "old body");
    doc.embedding = Some(vec![1.0; DEFAULT_EMBEDDING_DIM]);
    let id = store.memory().store(&doc).await.expect("test result");

    assert!(
        store
            .memory()
            .update_body(&id, "new body".to_owned())
            .await
            .expect("test result")
    );
    let updated = store
        .memory()
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(updated.embedding, None);
    let hits = store
        .memory()
        .search(
            &crabgent_core::SearchQuery::new("new")
                .scope(alice_scope())
                .embedding(vec![1.0; DEFAULT_EMBEDDING_DIM]),
        )
        .await
        .expect("test result");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].cosine_similarity, None);
}

#[tokio::test]
async fn update_body_with_embedding_sqlite_replaces_embedding() {
    let store = store().await;
    let mut doc = MemoryDoc::new(alice_scope(), "old body");
    doc.embedding = Some(vec![1.0; DEFAULT_EMBEDDING_DIM]);
    let id = store.memory().store(&doc).await.expect("test result");
    let replacement = vec![0.5; DEFAULT_EMBEDDING_DIM];

    assert!(
        store
            .memory()
            .update_body_with_embedding(&id, "new body".to_owned(), Some(replacement.clone()))
            .await
            .expect("test result")
    );
    let updated = store
        .memory()
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(updated.body, "new body");
    assert_eq!(updated.embedding, Some(replacement.clone()));
    let hits = store
        .memory()
        .search(
            &crabgent_core::SearchQuery::new("new")
                .scope(alice_scope())
                .embedding(replacement),
        )
        .await
        .expect("test result");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].cosine_similarity.is_some());
}
