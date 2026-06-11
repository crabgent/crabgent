mod common;

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use crabgent_core::{Tool, ToolError};
use crabgent_store::{MemoryTaskStore, Page, StoreError, Task, TaskId, TaskStatus, TaskStore};
use crabgent_task::TaskExecutor;
use serde_json::json;

use common::{
    HangingProvider, build_harness, build_immediate_harness, create_args, ctx, id_args,
    insert_task, test_task,
};

#[tokio::test]
async fn list_returns_running_tasks_for_owner() {
    let h = build_immediate_harness();
    let alice_running = insert_task(&h.store, test_task("alice", TaskStatus::Running, None)).await;
    insert_task(&h.store, test_task("bob", TaskStatus::Running, None)).await;
    insert_task(&h.store, test_task("alice", TaskStatus::Done, None)).await;

    let out = h
        .tool
        .execute(json!({"op": "list", "owner": "alice"}), &ctx())
        .await
        .expect("list succeeds");

    assert_eq!(out["count"], 1);
    assert_eq!(out["tasks"][0]["task_id"], alice_running.id.to_string());
    assert_eq!(out["tasks"][0]["owner"], "alice");
}

#[tokio::test]
async fn get_returns_task_by_id_or_not_found() {
    let h = build_immediate_harness();
    let task = insert_task(&h.store, test_task("alice", TaskStatus::Done, None)).await;

    let out = h
        .tool
        .execute(id_args("get", &task.id), &ctx())
        .await
        .expect("get succeeds");

    assert_eq!(out["task"]["task_id"], task.id.to_string());
    assert_eq!(out["task"]["status"], "done");
    let missing = h
        .tool
        .execute(id_args("get", &TaskId::new()), &ctx())
        .await
        .expect_err("missing task");
    assert!(matches!(missing, ToolError::NotFound(_)));
}

#[tokio::test]
async fn cancel_marks_task_failed_and_aborts_run() {
    let (provider, started) = HangingProvider::new();
    let h = build_harness(
        provider,
        |store| TaskExecutor::new(store).with_shutdown_grace(Duration::from_millis(20)),
        Arc::new(crabgent_core::AllowAllPolicy),
    );

    let out = h
        .tool
        .execute(create_args("hang"), &ctx())
        .await
        .expect("create succeeds");
    let id: TaskId = out["task_id"]
        .as_str()
        .expect("task id")
        .parse()
        .expect("value should parse");
    tokio::time::timeout(Duration::from_secs(1), started)
        .await
        .expect("provider starts")
        .expect("started signal");

    let cancelled = h
        .tool
        .execute(id_args("cancel", &id), &ctx())
        .await
        .expect("cancel succeeds");

    assert_eq!(cancelled["cancelled"], true);
    assert_eq!(cancelled["status"], "failed");
    let keys: BTreeSet<_> = cancelled
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, BTreeSet::from(["cancelled", "status", "task_id"]));
}

#[tokio::test]
async fn cancel_waits_for_executor_default_shutdown_grace() {
    let (provider, started) = HangingProvider::new();
    let store = Arc::new(DelayedVisibleFinishStore::new(Duration::from_millis(1_100)));
    let h = common::build_harness_for_store(
        Arc::clone(&store),
        provider,
        Arc::new(crabgent_core::AllowAllPolicy),
    );

    let out = h
        .tool
        .execute(create_args("slow cancel"), &ctx())
        .await
        .expect("create succeeds");
    let id: TaskId = out["task_id"]
        .as_str()
        .expect("task id")
        .parse()
        .expect("value should parse");
    tokio::time::timeout(Duration::from_secs(1), started)
        .await
        .expect("provider starts")
        .expect("started signal");

    let cancelled = h
        .tool
        .execute(id_args("cancel", &id), &ctx())
        .await
        .expect("cancel succeeds");

    assert_eq!(cancelled["cancelled"], true);
    assert_eq!(cancelled["status"], "failed");
}

struct DelayedVisibleFinishStore {
    inner: MemoryTaskStore,
    delay: Duration,
    mask_until: Mutex<HashMap<TaskId, Instant>>,
}

impl DelayedVisibleFinishStore {
    fn new(delay: Duration) -> Self {
        Self {
            inner: MemoryTaskStore::default(),
            delay,
            mask_until: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl TaskStore for DelayedVisibleFinishStore {
    async fn insert(&self, task: &Task) -> Result<(), StoreError> {
        self.inner.insert(task).await
    }

    async fn get(&self, id: &TaskId) -> Result<Option<Task>, StoreError> {
        let mut task = self.inner.get(id).await?;
        let until = self.mask_until.lock().expect("mask lock").get(id).copied();
        if let (Some(task), Some(until)) = (task.as_mut(), until)
            && Instant::now() < until
            && !matches!(task.status, TaskStatus::Running)
        {
            task.status = TaskStatus::Running;
            task.error = None;
            task.finished_at = None;
        }
        Ok(task)
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
        self.inner.finish(id, status, error).await?;
        if !matches!(status, TaskStatus::Running) {
            self.mask_until
                .lock()
                .expect("mask lock")
                .insert(id.clone(), Instant::now() + self.delay);
        }
        Ok(())
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
