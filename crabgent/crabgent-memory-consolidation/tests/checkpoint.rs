mod common;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use crabgent_core::policy::AllowAllPolicy;
use crabgent_memory_consolidation::{
    CLASS_CONSOLIDATION_CHECKPOINT, ConflictDecision, ConsolidationCheckpoint, ConsolidationError,
    Deduplicator, ExtractedFact, FactExtractor, StaleCleaner,
};
use crabgent_store::{MemoryDoc, MemoryMemoryStore, MemoryStore, StoreError};
use tokio_util::sync::CancellationToken;

use common::{
    StaticConflictResolver, episodic_doc, fact, long_body, runner_with, scope, store_doc, subject,
    token,
};

struct FailingFactExtractor;

#[async_trait]
impl FactExtractor for FailingFactExtractor {
    async fn extract(
        &self,
        _doc: &MemoryDoc,
        _token: &CancellationToken,
    ) -> Result<Vec<ExtractedFact>, ConsolidationError> {
        Err(StoreError::backend("pipeline failure marker").into())
    }
}

async fn store_checkpoint(
    store: &MemoryMemoryStore,
    checkpoint: &ConsolidationCheckpoint,
    updated_at: chrono::DateTime<Utc>,
) {
    let mut doc = MemoryDoc::new(
        scope(),
        serde_json::to_string(checkpoint).expect("serialize checkpoint"),
    );
    doc.class = Some(CLASS_CONSOLIDATION_CHECKPOINT.to_owned());
    doc.updated_at = updated_at;
    store_doc(store, &doc).await;
}

#[tokio::test]
async fn checkpoint_resumes_from_last_run() {
    let store = Arc::new(MemoryMemoryStore::default());
    let now = Utc::now();
    let checkpoint = ConsolidationCheckpoint {
        last_run_at: Some(now - Duration::hours(1)),
        ..ConsolidationCheckpoint::default()
    };
    store_checkpoint(&store, &checkpoint, now).await;
    let mut old = episodic_doc(long_body("old"));
    old.updated_at = now - Duration::hours(2);
    store_doc(&store, &old).await;
    let mut new = episodic_doc(long_body("new"));
    new.updated_at = now;
    store_doc(&store, &new).await;
    let runner = runner_with(
        store,
        vec![fact("new durable semantic fact")],
        ConflictDecision::Skip,
    );

    let result = runner
        .run(&subject(), scope(), token())
        .await
        .expect("test result");

    assert_eq!(result.sessions_processed, 1);
}

#[tokio::test]
async fn concurrent_run_returns_already_running() {
    let store = Arc::new(MemoryMemoryStore::default());
    let checkpoint = ConsolidationCheckpoint {
        in_progress: true,
        ..ConsolidationCheckpoint::default()
    };
    store_checkpoint(&store, &checkpoint, Utc::now()).await;
    let runner = runner_with(store, Vec::new(), ConflictDecision::Skip);

    let err = runner
        .run(&subject(), scope(), token())
        .await
        .expect_err("expected error");

    assert!(matches!(err, ConsolidationError::AlreadyRunning(_)));
}

#[tokio::test]
async fn concurrent_run_does_not_archive_stale_docs_before_lock_check() {
    let store = Arc::new(MemoryMemoryStore::default());
    let checkpoint = ConsolidationCheckpoint {
        in_progress: true,
        ..ConsolidationCheckpoint::default()
    };
    store_checkpoint(&store, &checkpoint, Utc::now()).await;
    let mut stale_doc = episodic_doc(long_body("stale"));
    stale_doc.importance = Some(0.1);
    stale_doc.updated_at = Utc::now() - Duration::days(31);
    let stale_id = stale_doc.id.clone();
    store_doc(&store, &stale_doc).await;
    let runner = runner_with(Arc::clone(&store), Vec::new(), ConflictDecision::Skip);

    let err = runner
        .run(&subject(), scope(), token())
        .await
        .expect_err("expected error");
    let stored = store
        .get(&stale_id)
        .await
        .expect("load stale doc")
        .expect("stale doc exists");

    assert!(matches!(err, ConsolidationError::AlreadyRunning(_)));
    assert!(
        stored.archived_at.is_none(),
        "AlreadyRunning must return before stale cleanup mutates docs"
    );
}

#[tokio::test]
async fn stale_checkpoint_lock_ignored() {
    let store = Arc::new(MemoryMemoryStore::default());
    let checkpoint = ConsolidationCheckpoint {
        in_progress: true,
        ..ConsolidationCheckpoint::default()
    };
    store_checkpoint(&store, &checkpoint, Utc::now() - Duration::seconds(3601)).await;
    let runner = runner_with(store, Vec::new(), ConflictDecision::Skip);

    let result = runner
        .run(&subject(), scope(), token())
        .await
        .expect("test result");

    assert_eq!(result.sessions_processed, 0);
}

#[tokio::test]
async fn runner_releases_checkpoint_on_pipeline_error() {
    let store = Arc::new(MemoryMemoryStore::default());
    store_doc(&store, &episodic_doc(long_body("failing"))).await;
    let store_dyn: Arc<dyn MemoryStore> = store.clone();
    let config = crabgent_memory_consolidation::ConsolidationConfig::default();
    let runner = crabgent_memory_consolidation::ConsolidationRunner::new(
        store_dyn.clone(),
        Arc::new(FailingFactExtractor),
        Deduplicator::new(store_dyn.clone()),
        Arc::new(StaticConflictResolver::new(ConflictDecision::Skip)),
        StaleCleaner::new(store_dyn.clone(), config.stale_policy.clone()),
        Arc::new(AllowAllPolicy),
        config,
    );

    let err = runner
        .run(&subject(), scope(), token())
        .await
        .expect_err("expected error");
    let checkpoint_hit = store
        .search(
            &crabgent_core::SearchQuery::new("")
                .scope(scope())
                .class(CLASS_CONSOLIDATION_CHECKPOINT),
        )
        .await
        .expect("test result")
        .into_iter()
        .next()
        .expect("checkpoint written");
    let checkpoint_doc = store
        .get(&checkpoint_hit.id)
        .await
        .expect("test result")
        .expect("test result");
    let checkpoint: ConsolidationCheckpoint =
        serde_json::from_str(&checkpoint_doc.body).expect("checkpoint json");

    assert!(matches!(err, ConsolidationError::Store(_)));
    assert!(!checkpoint.in_progress);
    assert_eq!(checkpoint.last_run_at, None);
    assert_eq!(checkpoint.sessions_processed, 0);

    let runner = runner_with(
        store,
        vec![fact("durable semantic fact after retry")],
        ConflictDecision::Skip,
    );
    let result = runner
        .run(&subject(), scope(), token())
        .await
        .expect("retry should process original doc");

    assert_eq!(result.sessions_processed, 1);
}
