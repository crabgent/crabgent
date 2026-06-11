use secrecy::SecretString;

use crabgent_core::{MemoryScope, Owner};
use crabgent_store::{MemoryDoc, MemoryStore};
use crabgent_store_postgres::{DEFAULT_EMBEDDING_DIM, PostgresStore, PostgresStoreConfig};

async fn maybe_store() -> Option<PostgresStore> {
    let dsn = std::env::var("PG_TEST_DSN").ok()?;
    let config = PostgresStoreConfig::from_secret(SecretString::from(dsn));
    Some(
        PostgresStore::open(config)
            .await
            .expect("open postgres store"),
    )
}

fn scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new(format!(
        "pg-memory-update-{}",
        uuid::Uuid::now_v7()
    )))
}

#[tokio::test]
async fn update_body_postgres_roundtrip() {
    let Some(store) = maybe_store().await else {
        return;
    };
    let scope = scope();
    let id = store
        .memory_store()
        .store(&MemoryDoc::new(scope, "old body"))
        .await
        .expect("test result");

    assert!(
        store
            .memory_store()
            .update_body(&id, "new body".to_owned())
            .await
            .expect("test result")
    );
    let updated = store
        .memory_store()
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(updated.body, "new body");
}

#[tokio::test]
async fn update_body_postgres_clears_embedding() {
    let Some(store) = maybe_store().await else {
        return;
    };
    let scope = scope();
    let mut doc = MemoryDoc::new(scope, "old body");
    doc.embedding = Some(vec![1.0; DEFAULT_EMBEDDING_DIM]);
    let id = store.memory_store().store(&doc).await.expect("test result");

    assert!(
        store
            .memory_store()
            .update_body(&id, "new body".to_owned())
            .await
            .expect("test result")
    );
    let updated = store
        .memory_store()
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(updated.embedding, None);
}

#[tokio::test]
async fn update_body_with_embedding_postgres_replaces_embedding() {
    let Some(store) = maybe_store().await else {
        return;
    };
    let scope = scope();
    let mut doc = MemoryDoc::new(scope, "old body");
    doc.embedding = Some(vec![1.0; DEFAULT_EMBEDDING_DIM]);
    let id = store.memory_store().store(&doc).await.expect("test result");
    let replacement = vec![0.5; DEFAULT_EMBEDDING_DIM];

    assert!(
        store
            .memory_store()
            .update_body_with_embedding(&id, "new body".to_owned(), Some(replacement.clone()))
            .await
            .expect("test result")
    );
    let updated = store
        .memory_store()
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert_eq!(updated.body, "new body");
    assert_eq!(updated.embedding, Some(replacement));
}
