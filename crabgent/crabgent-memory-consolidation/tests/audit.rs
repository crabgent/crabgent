mod common;

use std::sync::Arc;

use crabgent_core::SearchQuery;
use crabgent_memory_consolidation::{CLASS_CONSOLIDATION_AUDIT, ConflictDecision};
use crabgent_store::{MemoryMemoryStore, MemoryStore};

use common::{
    episodic_doc, fact, long_body, runner_with, scope, semantic_doc, store_doc, subject, token,
};

#[tokio::test]
async fn audit_does_not_leak_source_body() {
    let store = Arc::new(MemoryMemoryStore::default());
    let source_body = "alpha beta gamma delta secret-source-body-marker-12345";
    store_doc(&store, &semantic_doc(source_body)).await;
    store_doc(&store, &episodic_doc(long_body("episodic source"))).await;
    let runner = runner_with(
        store.clone(),
        vec![fact("alpha beta gamma delta")],
        ConflictDecision::KeepExisting,
    );

    let result = runner
        .run(&subject(), scope(), token())
        .await
        .expect("test result");
    let audits = store
        .search(
            &SearchQuery::new("")
                .scope(scope())
                .class(CLASS_CONSOLIDATION_AUDIT),
        )
        .await
        .expect("test result");

    assert_eq!(result.audits_written, 1);
    assert_eq!(audits.len(), 1);
    assert!(!audits[0].body.contains("secret-source-body-marker-12345"));
}
