mod common;

use std::sync::Arc;
use std::time::Duration;

use crabgent_task::TaskExecutor;
use tokio_util::sync::CancellationToken; // test-helper

use common::{HangingProvider, build_kernel, task_request};

#[tokio::test]
async fn executor_spawn_under_cap_rejects_when_cancelled_during_acquire() {
    let store = Arc::new(crabgent_store::MemoryTaskStore::default());
    let parent_cancel = CancellationToken::new();
    let executor = Arc::new(
        TaskExecutor::new(Arc::clone(&store))
            .with_max_parallel(1)
            .with_cancel(&parent_cancel)
            .with_shutdown_grace(Duration::from_millis(20)),
    );
    let (provider, started) = HangingProvider::new();
    let kernel = build_kernel(provider);
    executor
        .spawn(Arc::clone(&kernel), task_request("hold slot"))
        .await
        .expect("first spawn");
    tokio::time::timeout(Duration::from_secs(1), started)
        .await
        .expect("provider starts")
        .expect("started signal");

    let blocked = {
        let executor = Arc::clone(&executor);
        let kernel = Arc::clone(&kernel);
        tokio::spawn(async move { executor.spawn(kernel, task_request("blocked")).await })
    };
    parent_cancel.cancel();
    let err = tokio::time::timeout(Duration::from_secs(1), blocked)
        .await
        .expect("spawn returns")
        .expect("join ok")
        .expect_err("cancelled acquire");

    assert!(err.to_string().contains("shutting down"));
}
