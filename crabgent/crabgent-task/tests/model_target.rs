use std::sync::Arc;
use std::time::Duration;

use crabgent_core::{AllowAllPolicy, Kernel, ModelInfo, ModelTarget};
use crabgent_store::Owner;
use crabgent_store::memory::MemoryTaskStore;
use crabgent_store::records::{Task, TaskStatus};
use crabgent_store::traits::TaskStore;
use crabgent_task::{TaskExecutor, TaskRequest};
use crabgent_test_support::StubProvider;

/// A `StubProvider` named `provider` that advertises a single `opus` model and
/// echoes the provider name as its response text, so routing to a specific
/// provider is observable through the task output.
fn same_model_provider(provider: &'static str) -> StubProvider {
    StubProvider::with_text(provider)
        .with_name(provider)
        .with_models(vec![ModelInfo::minimal("opus", provider)])
}

async fn wait_for_done(store: &MemoryTaskStore, id: &crabgent_store::TaskId) -> Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let task = store
            .get(id)
            .await
            .expect("load task")
            .expect("task exists");
        if task.status == TaskStatus::Done {
            return task;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task did not finish"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_request_preserves_provider_qualified_model_target() {
    let store = Arc::new(MemoryTaskStore::default());
    let kernel = Arc::new(
        Kernel::builder()
            .provider(same_model_provider("anthropic"))
            .provider(same_model_provider("openai"))
            .policy(AllowAllPolicy)
            .build(),
    );
    let exec = TaskExecutor::new(Arc::clone(&store))
        .with_timeout(Duration::from_secs(5))
        .with_progress_debounce(Duration::from_millis(10));
    let req = TaskRequest::new(
        Owner::new("alice"),
        ModelTarget::new("openai", "opus"),
        "say hi",
    );

    let id = exec.spawn(kernel, req).await.expect("spawn task");
    let task = wait_for_done(&store, &id).await;

    assert_eq!(task.output, "openai");
}
