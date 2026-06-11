mod common;

use std::sync::Arc;

use crabgent_core::SearchQuery;
use crabgent_memory_consolidation::{
    ConflictDecision, ConsolidationAudit, Deduplicator, MAX_AUDIT_REASON_BYTES,
};
use crabgent_store::{MemoryMemoryStore, MemoryStore};
use proptest::prelude::*;

use common::{StaticConflictResolver, fact, scope, token};

prop_compose! {
    fn fact_body()(words in prop::collection::vec("[a-z]{1,8}", 1..=5)) -> String {
        words.join(" ")
    }
}

async fn run_dedup_pass(store: Arc<MemoryMemoryStore>, facts: &[String]) {
    let dedup = Deduplicator::new(store);
    let resolver = StaticConflictResolver::new(ConflictDecision::Skip);
    for body in facts {
        dedup
            .dedup(&fact(body), &scope(), &resolver, &token())
            .await
            .expect("dedup succeeds");
    }
}

async fn semantic_bodies(store: &MemoryMemoryStore) -> Vec<String> {
    let mut bodies: Vec<String> = store
        .search(&SearchQuery::new("").scope(scope()).class("semantic"))
        .await
        .expect("search succeeds")
        .into_iter()
        .map(|hit| hit.body)
        .collect();
    bodies.sort();
    bodies
}

proptest! {
    #[test]
    fn dedup_pipeline_is_stable(facts in prop::collection::vec(fact_body(), 1..=12)) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime");
        runtime.block_on(async move {
            let store = Arc::new(MemoryMemoryStore::default());
            run_dedup_pass(store.clone(), &facts).await;
            let after_first = semantic_bodies(&store).await;

            run_dedup_pass(store.clone(), &facts).await;
            let after_second = semantic_bodies(&store).await;

            prop_assert_eq!(after_second, after_first);
            Ok(())
        })?;
    }

    #[test]
    fn audit_reason_always_under_512_chars(reason in ".{0,2048}") {
        let audit = ConsolidationAudit::new(
            ConflictDecision::Skip,
            reason,
            Vec::new(),
            None,
            chrono::Utc::now(),
        );

        prop_assert!(audit.reason.len() <= MAX_AUDIT_REASON_BYTES);
        prop_assert!(audit.reason.chars().count() <= 512);
    }
}
