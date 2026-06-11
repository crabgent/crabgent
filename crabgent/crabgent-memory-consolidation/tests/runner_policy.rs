mod common;

use std::sync::Arc;

use crabgent_memory_consolidation::ConsolidationError;
use crabgent_store::MemoryMemoryStore;

use common::{denying_runner, scope, subject, token};

#[tokio::test]
async fn runner_respects_policy_deny() {
    let store = Arc::new(MemoryMemoryStore::default());
    let runner = denying_runner(store);

    let err = runner
        .run(&subject(), scope(), token())
        .await
        .expect_err("expected error");

    assert!(matches!(err, ConsolidationError::Denied(_)));
}
