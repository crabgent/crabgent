use crabgent_core::MemoryScope;
use crabgent_core::Owner;
use crabgent_store::{InMemoryStore, SessionId, SessionStore, Store, StoreError};

#[tokio::test]
async fn in_memory_store_round_trips_compaction_summary() {
    let store = InMemoryStore::new();
    let owner = Owner::new("u-summary");
    let session = store
        .session()
        .find_or_create(&owner, None, &MemoryScope::default())
        .await
        .expect("create session");

    store
        .session()
        .set_compaction_summary(&session.id, "prior compacted state")
        .await
        .expect("set compaction summary");

    let summary = store
        .session()
        .get_compaction_summary(&session.id)
        .await
        .expect("get compaction summary");
    assert_eq!(summary.as_deref(), Some("prior compacted state"));
}

#[tokio::test]
async fn in_memory_store_reports_missing_compaction_summary_session() {
    let store = InMemoryStore::new();
    let err = store
        .session()
        .set_compaction_summary(&SessionId::new(), "missing")
        .await
        .expect_err("missing session");

    assert!(matches!(err, StoreError::NotFound));
}
