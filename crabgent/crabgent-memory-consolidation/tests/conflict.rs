mod common;

use std::sync::Arc;

use crabgent_memory_consolidation::{
    ConflictDecision, ConflictResolver, Deduplicator, LlmConflictResolver,
};
use crabgent_store::{MemoryMemoryStore, MemoryStore};

use common::{MockProvider, StaticConflictResolver, fact, scope, semantic_doc, store_doc, token};

#[tokio::test]
async fn conflict_resolver_replace_decision_updates_winner() {
    let store = Arc::new(MemoryMemoryStore::default());
    let existing = semantic_doc("alpha beta gamma delta epsilon");
    let id = existing.id.clone();
    store_doc(&store, &existing).await;
    let dedup = Deduplicator::new(store.clone());

    let result = dedup
        .dedup(
            &fact("alpha beta gamma delta"),
            &scope(),
            &StaticConflictResolver::new(ConflictDecision::Replace),
            &token(),
        )
        .await
        .expect("test result");
    let updated = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert!(result.updated);
    assert_eq!(updated.body, "alpha beta gamma delta");
}

#[tokio::test]
async fn conflict_resolver_keep_existing_skips_new() {
    let store = Arc::new(MemoryMemoryStore::default());
    let existing = semantic_doc("alpha beta gamma delta epsilon");
    let id = existing.id.clone();
    store_doc(&store, &existing).await;
    let dedup = Deduplicator::new(store.clone());

    let result = dedup
        .dedup(
            &fact("alpha beta gamma delta"),
            &scope(),
            &StaticConflictResolver::new(ConflictDecision::KeepExisting),
            &token(),
        )
        .await
        .expect("test result");
    let kept = store
        .get(&id)
        .await
        .expect("test result")
        .expect("doc exists");

    assert!(result.conflict);
    assert!(!result.created);
    assert_eq!(kept.body, "alpha beta gamma delta epsilon");
}

#[tokio::test]
async fn conflict_resolver_provider_error_falls_back_to_skip() {
    let provider = Arc::new(MockProvider::provider_error_returns());
    let resolver = LlmConflictResolver::new(provider, "mock-model");
    let existing = semantic_doc("alpha beta gamma delta epsilon");

    let result = resolver
        .resolve(&existing, &fact("alpha beta gamma delta"), &token())
        .await
        .expect("test result");

    assert_eq!(result.decision, ConflictDecision::Skip);
}
