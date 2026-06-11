#![cfg(feature = "test-helpers")]

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use crabgent_core::{MemoryScope, Owner, SearchQuery};
use crabgent_memory::{
    EpisodicBlend, MemoryRecall, MockClock, SemanticBlend, recall_with_strategy,
};
use crabgent_store::{MemoryDoc, MemoryMemoryStore, MemoryStore};

fn scoped_doc(scope: MemoryScope, body: &str, created_at: chrono::DateTime<Utc>) -> MemoryDoc {
    let mut doc = MemoryDoc::new(scope, body);
    doc.created_at = created_at;
    doc.updated_at = created_at;
    doc
}

fn score_for_body(hits: &[crabgent_store::MemoryHit], body: &str) -> f32 {
    hits.iter()
        .find(|hit| hit.body == body)
        .expect("memory hit exists")
        .score
}

#[tokio::test]
async fn recall_strategies_rescore_memory_store_hits_with_mock_clock() {
    tokio::time::pause();

    let store = MemoryMemoryStore::default();
    let base = Utc
        .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
        .single()
        .expect("valid test datetime");
    let clock = MockClock::new(base);
    let scope = MemoryScope::for_owner(Owner::new("recall-integration"));

    let mut semantic = scoped_doc(scope.clone(), "alpha semantic", base - Duration::hours(1));
    semantic.class = Some("semantic".to_owned());
    semantic.importance = Some(0.9);
    store.store(&semantic).await.expect("test result");

    let mut episodic = scoped_doc(scope.clone(), "alpha episodic", base);
    episodic.class = Some("episodic".to_owned());
    episodic.importance = Some(0.9);
    store.store(&episodic).await.expect("test result");

    let query = SearchQuery::new("alpha").scope(scope);
    let semantic_strategy = SemanticBlend::default();
    let semantic_hits = recall_with_strategy(&store, &semantic_strategy, &clock, &query)
        .await
        .expect("test result");
    assert_eq!(semantic_hits.len(), 2);

    let episodic_strategy = EpisodicBlend::default();
    let episodic_before = recall_with_strategy(&store, &episodic_strategy, &clock, &query)
        .await
        .expect("test result");
    clock.advance(Duration::hours(48));
    let episodic_after = recall_with_strategy(&store, &episodic_strategy, &clock, &query)
        .await
        .expect("test result");
    assert!(
        score_for_body(&episodic_before, "alpha episodic")
            > score_for_body(&episodic_after, "alpha episodic")
    );
}

#[tokio::test]
async fn memory_recall_selects_strategy_from_doc_class() {
    let store = Arc::new(MemoryMemoryStore::default());
    let base = Utc
        .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
        .single()
        .expect("valid test datetime");
    let clock = Arc::new(MockClock::new(base));
    let scope = MemoryScope::for_owner(Owner::new("recall-interface"));

    // Both docs share class="episodic", so MemoryRecall must resolve the
    // class string to EpisodicBlend via from_str -> defaults_for. Episodic
    // applies time decay, so the recent doc outscores the older one;
    // SemanticBlend would ignore time and score them equally.
    let mut recent = scoped_doc(scope.clone(), "needle recent", base);
    recent.class = Some("episodic".to_owned());
    store.store(&recent).await.expect("test result");

    let mut old = scoped_doc(scope.clone(), "needle old", base - Duration::hours(30));
    old.class = Some("episodic".to_owned());
    store.store(&old).await.expect("test result");

    let store_dyn: Arc<dyn MemoryStore> = store;
    let recall = MemoryRecall::with_clock(store_dyn, clock);
    let hits = recall
        .search(&SearchQuery::new("needle").scope(scope))
        .await
        .expect("test result");

    assert_eq!(
        hits.first().map(|hit| hit.body.as_str()),
        Some("needle recent")
    );
    assert!(
        score_for_body(&hits, "needle recent") > score_for_body(&hits, "needle old"),
        "episodic time decay must rank the recent doc above the old one"
    );
}

#[tokio::test]
async fn unclassed_hit_does_not_outrank_rescored_hit_in_mixed_set() {
    // Mixed-class result set: one doc has a recognized class (semantic) and is
    // rescored onto the blend scale; the other has no class. Without
    // normalization the unclassed doc keeps its raw backend score (1.0 from the
    // in-memory backend) and would tie or beat the rescored hit. The neutral
    // baseline must bring it onto the same scale so it ranks below the
    // higher-importance classed hit.
    let store = Arc::new(MemoryMemoryStore::default());
    let base = Utc
        .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
        .single()
        .expect("valid test datetime");
    let clock = Arc::new(MockClock::new(base));
    let scope = MemoryScope::for_owner(Owner::new("mixed-class"));

    // Classed semantic doc, importance 1.0: SemanticBlend = 0.7*1 + 0.2*1 = 0.9.
    let mut classed = scoped_doc(scope.clone(), "topic classed", base);
    classed.class = Some("semantic".to_owned());
    classed.importance = Some(1.0);
    store.store(&classed).await.expect("store classed");

    // Unclassed doc, importance 0.5: neutral baseline = 0.7*1 + 0.2*0.5 = 0.8,
    // strictly below the classed hit. Created later so the created_at tiebreaker
    // would have favored it under the old raw-score tie.
    let unclassed = scoped_doc(scope.clone(), "topic unclassed", base + Duration::hours(1));
    store.store(&unclassed).await.expect("store unclassed");

    let store_dyn: Arc<dyn MemoryStore> = store;
    let recall = MemoryRecall::with_clock(store_dyn, clock);
    let hits = recall
        .search(&SearchQuery::new("topic").scope(scope))
        .await
        .expect("recall search");

    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits.first().map(|hit| hit.body.as_str()),
        Some("topic classed"),
        "rescored classed hit must rank above the unclassed hit"
    );
    assert!(
        score_for_body(&hits, "topic classed") > score_for_body(&hits, "topic unclassed"),
        "unclassed hit must be normalized below the rescored hit"
    );
}
