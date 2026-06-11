use std::sync::Arc;
use std::time::Duration;

use crabgent_core::{AllowAllPolicy, Kernel, KernelBuilder};
use crabgent_store::{MemoryTaskStore, Owner, TaskId, TaskStatus};
use crabgent_task::{TaskExecutor, TaskRequest};
use crabgent_test_support::StubProvider;

fn build_test_kernel() -> Arc<Kernel> {
    Arc::new(
        KernelBuilder::new()
            .provider(StubProvider::with_text("api smoke"))
            .policy(AllowAllPolicy)
            .build(),
    )
}

#[tokio::test]
async fn cancel_returns_false_for_unknown_id() {
    let store = Arc::new(MemoryTaskStore::default());
    let exec = TaskExecutor::new(store);

    assert!(!exec.cancel(&TaskId::new()).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_blocking_compiles_and_returns_result_from_external_crate() {
    let store = Arc::new(MemoryTaskStore::default());
    let exec = TaskExecutor::new(store);
    let req = TaskRequest::new(Owner::new("api"), "m", "run");

    let task = exec
        .spawn_blocking(build_test_kernel(), req, Some(Duration::from_secs(1)))
        .await
        .expect("spawn_blocking returns task");

    assert_eq!(task.status, TaskStatus::Done);
    assert_eq!(task.output, "api smoke");
}
