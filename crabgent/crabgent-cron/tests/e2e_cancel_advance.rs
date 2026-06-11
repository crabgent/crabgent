//! Regression test: a cron job cancelled mid-run must still have its claim
//! finished and schedule advanced, so the next tick can re-pick it instead of
//! leaving it wedged as claimed.
//!
//! Drives a real [`CronScheduler`] through its public API, parks the executor
//! on the per-run cancel token, fires an external shutdown cancel, then checks
//! the store: `claimed_at` cleared, `last_run` set (proving `advance_schedule`
//! finished the claim rather than merely releasing it), and `next_run`
//! advanced past the original due time.

use std::sync::Mutex;
use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_core::Kernel;
use crabgent_core::RunCtx;
use crabgent_core::error::ProviderError;
use crabgent_core::model::ModelInfo;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse};
use crabgent_core::{MemoryScope, Owner};
use crabgent_cron::{CronError, CronExecCtx, CronExecResult, CronExecutor, CronScheduler};
use crabgent_store::CronJobId;
use crabgent_store::memory::MemoryCronStore;
use crabgent_store::records::{CronJob, CronSchedule};
use crabgent_store::traits::CronStore;
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

struct PanicProvider;

#[async_trait]
impl Provider for PanicProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Other(
            "kernel must not be called by the cancel-aware executor".into(),
        ))
    }
    fn name(&self) -> &'static str {
        "panic"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("m", "panic")]
    }
}

/// Signals when it has started, then parks on the per-run cancel token and
/// returns cooperatively once cancelled.
struct CancelAwareExecutor {
    started: Mutex<Option<oneshot::Sender<()>>>,
    observed_cancel: Arc<AtomicBool>,
}

#[async_trait]
impl CronExecutor for CancelAwareExecutor {
    async fn execute(&self, ctx: CronExecCtx<'_>) -> Result<CronExecResult, CronError> {
        if let Some(tx) = self.started.lock().expect("started mutex").take()
            && tx.send(()).is_err()
        {
            // Receiver dropped after observing start; nothing to do.
        }
        ctx.cancel.cancelled().await;
        self.observed_cancel.store(true, Ordering::SeqCst);
        Ok(CronExecResult::default())
    }
}

fn due_job() -> CronJob {
    let stale = Utc::now() - chrono::Duration::seconds(120);
    CronJob {
        id: CronJobId::new(),
        name: "cancel-advance".into(),
        scope: MemoryScope::for_owner(Owner::new("telegram:1"))
            .with_channel("telegram")
            .with_conv("telegram:1"),
        prompt: "p".into(),
        schedule: CronSchedule::every(60),
        enabled: true,
        run_once: false,
        model_override: None,
        reasoning_effort_override: None,
        pre_command: None,
        delivery_ctx: json!({}),
        last_run: None,
        next_run: stale,
        created_at: stale,
        claimed_at: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_job_still_advances_schedule() {
    let store = Arc::new(MemoryCronStore::default());
    let job = due_job();
    let job_id = job.id.clone();
    let original_next = job.next_run;
    store.create(&job).await.expect("create due cron job");

    let kernel = Arc::new(
        Kernel::builder()
            .provider(PanicProvider)
            .policy(AllowAllPolicy)
            .build(),
    );
    let (started_tx, started_rx) = oneshot::channel();
    let observed_cancel = Arc::new(AtomicBool::new(false));
    let executor: Arc<dyn CronExecutor> = Arc::new(CancelAwareExecutor {
        started: Mutex::new(Some(started_tx)),
        observed_cancel: Arc::clone(&observed_cancel),
    });
    let scheduler = Arc::new(
        CronScheduler::new(Arc::clone(&store), kernel, executor)
            .with_tick_interval(Duration::from_millis(25))
            .with_job_timeout(Duration::from_mins(1))
            .with_job_cancel_grace(Duration::from_millis(25)),
    );
    let cancel = CancellationToken::new();
    let scheduler_handle = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        let cancel = cancel.clone();
        async move {
            scheduler.run(cancel).await.expect("scheduler exits clean");
        }
    });

    // Wait until the job is mid-run (executor parked on its cancel token).
    tokio::time::timeout(Duration::from_secs(5), started_rx)
        .await
        .expect("job should start within 5s")
        .expect("started sender should live");

    // External shutdown cancel propagates to the per-run token; the executor
    // returns cooperatively and the worker advances the schedule before exit.
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), scheduler_handle)
        .await
        .expect("scheduler should drain cancelled job within 5s")
        .expect("scheduler task should join");

    assert!(
        observed_cancel.load(Ordering::SeqCst),
        "executor must observe the cancellation"
    );

    let stored = store
        .get(&job_id)
        .await
        .expect("store get")
        .expect("job exists");
    assert!(
        stored.claimed_at.is_none(),
        "cancelled job must not stay claimed"
    );
    assert!(
        stored.last_run.is_some(),
        "advance_schedule must finish the claim (last_run set), not just release it"
    );
    assert!(
        stored.next_run > original_next,
        "schedule must advance past the original due time"
    );
    assert!(stored.enabled, "non-run_once cancelled job stays enabled");
}
