mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_core::{Owner, StrictPolicy, Tool, ToolError};
use crabgent_store::{MemoryTaskStore, Page, StoreError, Task, TaskId, TaskStatus, TaskStore};
use serde_json::json;

use common::{
    DenyNamedPolicy, ImmediateProvider, build_harness_for_store, ctx, id_args, insert_task,
    test_task,
};

#[derive(Default)]
struct CountingTaskStore {
    inner: MemoryTaskStore,
    gets: AtomicUsize,
}

impl CountingTaskStore {
    fn get_count(&self) -> usize {
        self.gets.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl TaskStore for CountingTaskStore {
    async fn insert(&self, task: &Task) -> Result<(), StoreError> {
        self.inner.insert(task).await
    }

    async fn get(&self, id: &TaskId) -> Result<Option<Task>, StoreError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.inner.get(id).await
    }

    async fn append_output(&self, id: &TaskId, chunk: &str) -> Result<(), StoreError> {
        self.inner.append_output(id, chunk).await
    }

    async fn finish(
        &self,
        id: &TaskId,
        status: TaskStatus,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        self.inner.finish(id, status, error).await
    }

    async fn list_running(&self, page: Page) -> Result<Vec<Task>, StoreError> {
        self.inner.list_running(page).await
    }

    async fn pause(
        &self,
        id: &TaskId,
        cause: crabgent_store::TaskPauseCause,
    ) -> Result<bool, StoreError> {
        self.inner.pause(id, cause).await
    }

    async fn claim_for_resume(&self, id: &TaskId, max_resumes: u32) -> Result<bool, StoreError> {
        self.inner.claim_for_resume(id, max_resumes).await
    }

    async fn list_paused(&self, page: Page) -> Result<Vec<Task>, StoreError> {
        self.inner.list_paused(page).await
    }

    async fn pause_orphans(&self, stale_secs: i64) -> Result<Vec<TaskId>, StoreError> {
        self.inner.pause_orphans(stale_secs).await
    }

    async fn list_children(&self, parent: &TaskId, page: Page) -> Result<Vec<Task>, StoreError> {
        self.inner.list_children(parent, page).await
    }

    async fn save_transcript(
        &self,
        id: &TaskId,
        messages: &[crabgent_core::Message],
    ) -> Result<(), StoreError> {
        self.inner.save_transcript(id, messages).await
    }

    async fn load_transcript(
        &self,
        id: &TaskId,
    ) -> Result<Option<Vec<crabgent_core::Message>>, StoreError> {
        self.inner.load_transcript(id).await
    }

    async fn recover_stuck(&self, timeout_secs: i64) -> Result<Vec<TaskId>, StoreError> {
        self.inner.recover_stuck(timeout_secs).await
    }

    async fn cleanup_old(&self, days: i64) -> Result<u64, StoreError> {
        self.inner.cleanup_old(days).await
    }
}

#[tokio::test]
async fn denied_get_cancel_do_not_touch_store() {
    for op in ["get", "cancel"] {
        let store = Arc::new(CountingTaskStore::default());
        let denied_tool = match op {
            "get" => "task.get",
            "cancel" => "task.cancel",
            _ => panic!("unexpected test op: {op}"),
        };
        let h = build_harness_for_store(
            store.clone(),
            ImmediateProvider::new("done"),
            Arc::new(DenyNamedPolicy::new([denied_tool])),
        );
        let missing_id = TaskId::new();

        let err = h
            .tool
            .execute(id_args(op, &missing_id), &ctx())
            .await
            .expect_err("denied before read");

        assert!(matches!(err, ToolError::Permission(_)));
        assert_eq!(store.get_count(), 0);
    }
}

#[tokio::test]
async fn owner_scoped_get_requires_requested_owner() {
    let policy = StrictPolicy::builder()
        .allow_task_any_for_owner(Owner::new("alice"))
        .build();
    let h = build_harness_for_store(
        Arc::new(MemoryTaskStore::default()),
        ImmediateProvider::new("done"),
        Arc::new(policy),
    );
    let task = insert_task(&h.store, test_task("alice", TaskStatus::Done, None)).await;

    let err = h
        .tool
        .execute(id_args("get", &task.id), &ctx())
        .await
        .expect_err("missing owner is denied before read");
    assert!(matches!(err, ToolError::Permission(_)));

    let output = h
        .tool
        .execute(
            json!({"op": "get", "task_id": task.id.to_string(), "owner": "alice"}),
            &ctx(),
        )
        .await
        .expect("owner-scoped get");
    assert_eq!(output["task"]["task_id"], task.id.to_string());
}

#[tokio::test]
async fn owner_mismatch_returns_not_found() {
    let h = common::build_immediate_harness();
    let task = insert_task(&h.store, test_task("alice", TaskStatus::Done, None)).await;

    let err = h
        .tool
        .execute(
            json!({"op": "get", "task_id": task.id.to_string(), "owner": "bob"}),
            &ctx(),
        )
        .await
        .expect_err("owner mismatch");

    assert!(matches!(err, ToolError::NotFound(_)));
}
