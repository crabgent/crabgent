use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_core::model::ModelInfo;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse, StopReason, Usage};
use crabgent_core::{Kernel, KernelBuilder, ProviderError, RunCtx};
use crabgent_store::memory::MemoryTaskStore;
use crabgent_store::{Owner, Page, StoreError, Task, TaskId, TaskStatus, TaskStore};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::blocking::do_spawn_blocking;
use super::{TIMEOUT_MESSAGE, TaskExecutor};
use crate::TaskRequest;

struct CancelAwareProvider {
    started: Mutex<Option<oneshot::Sender<()>>>,
}

#[async_trait]
impl Provider for CancelAwareProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let started = self.started.lock().expect("started lock").take();
        if let Some(tx) = started {
            let _receiver_dropped = tx.send(()).is_err();
        }
        cancel.expect("cancel token").cancelled().await;
        Ok(LlmResponse {
            text: "cancelled".to_owned(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "cancel-aware"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("claude-haiku-4-5", "cancel-aware")]
    }
}

struct FirstFinishMaskedStore {
    inner: MemoryTaskStore,
    masked_once: Mutex<HashSet<TaskId>>,
    mask_until: Mutex<Option<(TaskId, std::time::Instant)>>,
    delay: Duration,
}

impl FirstFinishMaskedStore {
    fn new(delay: Duration) -> Self {
        Self {
            inner: MemoryTaskStore::default(),
            masked_once: Mutex::new(HashSet::new()),
            mask_until: Mutex::new(None),
            delay,
        }
    }
}

#[async_trait]
impl TaskStore for FirstFinishMaskedStore {
    async fn insert(&self, task: &Task) -> Result<(), StoreError> {
        self.inner.insert(task).await
    }

    async fn get(&self, id: &TaskId) -> Result<Option<Task>, StoreError> {
        let mut task = self.inner.get(id).await?;
        let masked = self
            .mask_until
            .lock()
            .expect("mask lock")
            .as_ref()
            .is_some_and(|(masked_id, until)| {
                masked_id == id && std::time::Instant::now() < *until
            });
        if let Some(task) = task.as_mut()
            && masked
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
        if error == Some(TIMEOUT_MESSAGE) {
            self.mask_until
                .lock()
                .expect("mask lock")
                .take_if(|(masked_id, _until)| masked_id == id);
        }
        if !matches!(status, TaskStatus::Running)
            && error != Some(TIMEOUT_MESSAGE)
            && self
                .masked_once
                .lock()
                .expect("masked lock")
                .insert(id.clone())
        {
            self.mask_until
                .lock()
                .expect("mask lock")
                .replace((id.clone(), std::time::Instant::now() + self.delay));
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

fn request() -> TaskRequest {
    TaskRequest::new(Owner::new("alice"), "claude-haiku-4-5", "run")
}

fn kernel(started: oneshot::Sender<()>) -> Arc<Kernel> {
    Arc::new(
        KernelBuilder::new()
            .provider(CancelAwareProvider {
                started: Mutex::new(Some(started)),
            })
            .policy(AllowAllPolicy)
            .build(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocking_timeout_returns_failed_when_cancel_finish_is_not_visible_before_grace() {
    let store = Arc::new(FirstFinishMaskedStore::new(Duration::from_secs(1)));
    let (started_tx, started_rx) = oneshot::channel();
    let exec = TaskExecutor::new(Arc::clone(&store))
        .with_timeout(Duration::from_mins(1))
        .with_shutdown_grace(Duration::from_millis(10));

    let task = do_spawn_blocking(
        &exec,
        kernel(started_tx),
        request(),
        Some(Duration::from_millis(10)),
    )
    .await
    .expect("blocking spawn");

    started_rx.await.expect("provider started");
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(task.error.as_deref(), Some(TIMEOUT_MESSAGE));
}
