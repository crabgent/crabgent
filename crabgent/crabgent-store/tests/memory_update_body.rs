use crabgent_core::{MemoryId, MemoryScope, Owner};
use crabgent_store::{MemoryDoc, MemoryMemoryStore, MemoryStore};

fn alice_scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new("alice"))
        .with_channel("slack")
        .with_kind("direct")
}

#[tokio::test]
async fn update_body_inmemory_roundtrip() {
    let store = MemoryMemoryStore::default();
    let id = store
        .store(&MemoryDoc::new(alice_scope(), "old body"))
        .await
        .expect("test result");

    assert!(
        store
            .update_body(&id, "new body".to_owned())
            .await
            .expect("test result")
    );
    let updated = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(updated.body, "new body");
}

#[tokio::test]
async fn update_body_inmemory_not_found_returns_false() {
    let store = MemoryMemoryStore::default();

    assert!(
        !store
            .update_body(&MemoryId::new(), "new body".to_owned())
            .await
            .expect("test result")
    );
}

#[tokio::test]
async fn update_body_inmemory_preserves_scope() {
    let store = MemoryMemoryStore::default();
    let scope = alice_scope();
    let id = store
        .store(&MemoryDoc::new(scope.clone(), "old body"))
        .await
        .expect("test result");

    assert!(
        store
            .update_body(&id, "new body".to_owned())
            .await
            .expect("test result")
    );
    let updated = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(updated.scope, scope);
}

#[tokio::test]
async fn update_body_inmemory_clears_embedding() {
    let store = MemoryMemoryStore::default();
    let mut doc = MemoryDoc::new(alice_scope(), "old body");
    doc.embedding = Some(vec![1.0, 0.0, 0.0]);
    let id = store.store(&doc).await.expect("test result");

    assert!(
        store
            .update_body(&id, "new body".to_owned())
            .await
            .expect("test result")
    );
    let updated = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(updated.embedding, None);
}

#[tokio::test]
async fn update_body_with_embedding_inmemory_replaces_embedding() {
    let store = MemoryMemoryStore::default();
    let mut doc = MemoryDoc::new(alice_scope(), "old body");
    doc.embedding = Some(vec![1.0, 0.0, 0.0]);
    let id = store.store(&doc).await.expect("test result");

    assert!(
        store
            .update_body_with_embedding(&id, "new body".to_owned(), Some(vec![0.0, 1.0, 0.0]))
            .await
            .expect("test result")
    );
    let updated = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(updated.body, "new body");
    assert_eq!(updated.embedding, Some(vec![0.0, 1.0, 0.0]));
}
