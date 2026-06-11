mod common;

use std::sync::Arc;

use crabgent_core::SearchQuery;
use crabgent_memory::MemoryClass;
use crabgent_memory_consolidation::ConflictDecision;
use crabgent_store::{MemoryMemoryStore, MemoryStore};

use common::{episodic_doc, fact, long_body, runner_with, scope, store_doc, subject, token};

#[tokio::test]
async fn runner_with_empty_scope_returns_zero_stats() {
    let store = Arc::new(MemoryMemoryStore::default());
    let runner = runner_with(store, Vec::new(), ConflictDecision::Skip);

    let result = runner
        .run(&subject(), scope(), token())
        .await
        .expect("test result");

    assert_eq!(result.sessions_processed, 0);
    assert_eq!(result.facts_extracted, 0);
    assert_eq!(result.memories_created, 0);
}

#[tokio::test]
async fn runner_with_episodic_promotes_to_semantic() {
    let store = Arc::new(MemoryMemoryStore::default());
    store_doc(&store, &episodic_doc(long_body("Alice prefers tea"))).await;
    let runner = runner_with(
        store.clone(),
        vec![fact("Alice prefers tea in the afternoon")],
        ConflictDecision::Skip,
    );

    let result = runner
        .run(&subject(), scope(), token())
        .await
        .expect("test result");
    let hits = store
        .search(
            &SearchQuery::new("Alice prefers tea")
                .scope(scope())
                .class(MemoryClass::Semantic.as_str()),
        )
        .await
        .expect("test result");

    assert_eq!(result.sessions_processed, 1);
    assert_eq!(result.facts_extracted, 1);
    assert_eq!(result.memories_created, 1);
    assert_eq!(hits.len(), 1);
}
