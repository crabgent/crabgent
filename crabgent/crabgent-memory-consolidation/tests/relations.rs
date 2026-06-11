mod common;

use std::sync::Arc;

use crabgent_memory_consolidation::ConflictDecision;
use crabgent_store::{MemoryMemoryStore, MemoryStore, RelationType};

use common::{
    episodic_doc, fact, long_body, runner_with, scope, semantic_doc, store_doc, subject, token,
};

async fn neighbor_types(store: &MemoryMemoryStore) -> Vec<String> {
    let scope = scope();
    let ids: Vec<_> = store
        .search(
            &crabgent_core::SearchQuery::new("")
                .scope(scope.clone())
                .limit(100),
        )
        .await
        .expect("search ids")
        .into_iter()
        .map(|hit| hit.id)
        .collect();
    let mut types: Vec<String> = store
        .relation_neighbors(&ids, &scope)
        .await
        .expect("relation neighbors")
        .into_iter()
        .map(|rel| rel.relation_type.as_str().to_owned())
        .collect();
    types.sort();
    types.dedup();
    types
}

#[tokio::test]
async fn brand_new_fact_emits_derived_from_edge() {
    let store = Arc::new(MemoryMemoryStore::default());
    store_doc(&store, &episodic_doc(long_body("Alice prefers tea"))).await;
    let runner = runner_with(
        store.clone(),
        vec![fact("Alice prefers tea in the afternoon")],
        ConflictDecision::Skip,
    );

    runner
        .run(&subject(), scope(), token())
        .await
        .expect("run ok");

    assert_eq!(
        neighbor_types(&store).await,
        vec![RelationType::derived_from().as_str().to_owned()]
    );
}

#[tokio::test]
async fn high_similarity_restatement_emits_supports_edge() {
    let store = Arc::new(MemoryMemoryStore::default());
    store_doc(&store, &episodic_doc(long_body("Alice prefers tea"))).await;
    // Pre-store the identical semantic fact so the dedup hits the
    // high-similarity upsert path (no conflict).
    store_doc(&store, &semantic_doc("alpha beta gamma delta")).await;
    let runner = runner_with(
        store.clone(),
        vec![fact("alpha beta gamma delta")],
        ConflictDecision::Skip,
    );

    runner
        .run(&subject(), scope(), token())
        .await
        .expect("run ok");

    assert_eq!(
        neighbor_types(&store).await,
        vec![RelationType::supports().as_str().to_owned()]
    );
}

#[tokio::test]
async fn both_valid_conflict_emits_contradicts_edge() {
    let store = Arc::new(MemoryMemoryStore::default());
    store_doc(&store, &episodic_doc(long_body("Alice prefers tea"))).await;
    // A near-duplicate (conflict-zone similarity) pre-stored fact forces the
    // resolver path; BothValid keeps both and emits a contradicts edge.
    store_doc(&store, &semantic_doc("alpha beta gamma delta epsilon")).await;
    let runner = runner_with(
        store.clone(),
        vec![fact("alpha beta gamma delta")],
        ConflictDecision::BothValid,
    );

    runner
        .run(&subject(), scope(), token())
        .await
        .expect("run ok");

    assert_eq!(
        neighbor_types(&store).await,
        vec![RelationType::contradicts().as_str().to_owned()]
    );
}

#[tokio::test]
async fn replace_conflict_emits_supersedes_edge() {
    let store = Arc::new(MemoryMemoryStore::default());
    store_doc(&store, &episodic_doc(long_body("Alice prefers tea"))).await;
    store_doc(&store, &semantic_doc("alpha beta gamma delta epsilon")).await;
    let runner = runner_with(
        store.clone(),
        vec![fact("alpha beta gamma delta")],
        ConflictDecision::Replace,
    );

    runner
        .run(&subject(), scope(), token())
        .await
        .expect("run ok");

    assert_eq!(
        neighbor_types(&store).await,
        vec![RelationType::supersedes().as_str().to_owned()]
    );
}
