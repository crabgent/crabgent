use super::worker::{CANCELLED_MESSAGE, dispatch_deliveries, run_executor, run_pre_processors};
use super::*;

use std::future::pending;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::delivery::NoopDelivery;
use crate::executor::{CronExecCtx, CronExecResult, KernelCronExecutor};
use crate::observer::NoopCronObserver;
use crate::pre_processor::CronPreProcessResult;
use async_trait::async_trait;
use crabgent_core::RunCtx;
use crabgent_core::error::ProviderError;
use crabgent_core::model::ModelInfo;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse, StopReason, Usage};
use crabgent_core::{MemoryScope, Owner};
use crabgent_store::CronJobId;
use crabgent_store::memory::MemoryTaskStore;
use crabgent_store::records::CronSchedule;
use crabgent_store::traits::CronStore;
use serde_json::json;
use tokio::sync::oneshot;

struct StubProvider;
#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Ok(LlmResponse {
            text: "ok".into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }
    fn name(&self) -> &'static str {
        "stub"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("stub", "stub")]
    }
}

fn fixture(prompt: &str) -> CronJob {
    CronJob {
        id: CronJobId::new(),
        name: "demo".into(),
        scope: MemoryScope::default(),
        prompt: prompt.into(),
        schedule: CronSchedule::every(60),
        enabled: true,
        run_once: false,
        model_override: None,
        reasoning_effort_override: None,
        pre_command: None,
        delivery_ctx: json!({}),
        last_run: None,
        next_run: Utc::now(),
        created_at: Utc::now(),
        claimed_at: None,
    }
}

fn telegram_scoped_job() -> CronJob {
    let mut job = fixture("p");
    job.scope = MemoryScope::for_owner(Owner::new("telegram:478376391"))
        .with_channel("telegram")
        .with_conv("telegram:478376391");
    job
}

fn dummy_kernel() -> Arc<Kernel> {
    let _ = MemoryTaskStore::default();
    Arc::new(
        Kernel::builder()
            .provider(StubProvider)
            .policy(AllowAllPolicy)
            .build(),
    )
}

#[tokio::test]
async fn run_pre_processors_no_processors_passthrough() {
    let job = fixture("p");
    let result = run_pre_processors(&[], &job).await;
    assert!(matches!(result, CronPreProcessResult::Passthrough));
}

struct AlwaysSkip;
#[async_trait]
impl CronPreProcessor for AlwaysSkip {
    async fn pre_process(&self, _job: &CronJob) -> CronPreProcessResult {
        CronPreProcessResult::Skip
    }
}

struct AlwaysAugment;
#[async_trait]
impl CronPreProcessor for AlwaysAugment {
    async fn pre_process(&self, _job: &CronJob) -> CronPreProcessResult {
        CronPreProcessResult::RunLlm("[ctx]".into())
    }
}

struct AlwaysPass;
#[async_trait]
impl CronPreProcessor for AlwaysPass {
    async fn pre_process(&self, _job: &CronJob) -> CronPreProcessResult {
        CronPreProcessResult::Passthrough
    }
}

#[tokio::test]
async fn run_pre_processors_first_non_passthrough_wins() {
    let pps: Vec<Arc<dyn CronPreProcessor>> = vec![
        Arc::new(AlwaysPass),
        Arc::new(AlwaysSkip),
        Arc::new(AlwaysAugment),
    ];
    let job = fixture("p");
    let result = run_pre_processors(&pps, &job).await;
    assert!(matches!(result, CronPreProcessResult::Skip));
}

#[tokio::test]
async fn run_pre_processors_all_pass_returns_passthrough() {
    let pps: Vec<Arc<dyn CronPreProcessor>> = vec![Arc::new(AlwaysPass), Arc::new(AlwaysPass)];
    let job = fixture("p");
    let result = run_pre_processors(&pps, &job).await;
    assert!(matches!(result, CronPreProcessResult::Passthrough));
}

#[tokio::test]
async fn dispatch_deliveries_does_not_panic_on_empty() {
    let job = fixture("p");
    dispatch_deliveries(&[], &job, "msg")
        .await
        .expect("no deliveries");
}

#[tokio::test]
async fn dispatch_deliveries_calls_each() {
    let d: Vec<Arc<dyn CronDelivery>> = vec![Arc::new(NoopDelivery), Arc::new(NoopDelivery)];
    let job = telegram_scoped_job();
    dispatch_deliveries(&d, &job, "msg")
        .await
        .expect("delivery target resolves from scope");
}

struct RecordingDeliveryCtx {
    seen: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[async_trait]
impl CronDelivery for RecordingDeliveryCtx {
    async fn deliver(&self, job: &CronJob, _message: &str) -> Result<bool, CronError> {
        self.seen
            .lock()
            .expect("delivery ctx mutex")
            .push(job.delivery_ctx.clone());
        Ok(true)
    }
}

struct CountingDelivery {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl CronDelivery for CountingDelivery {
    async fn deliver(&self, _job: &CronJob, _message: &str) -> Result<bool, CronError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
}

#[tokio::test]
async fn dispatch_deliveries_reconstructs_empty_ctx_from_scope() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let d: Vec<Arc<dyn CronDelivery>> = vec![Arc::new(RecordingDeliveryCtx {
        seen: Arc::clone(&seen),
    })];
    let job = telegram_scoped_job();

    dispatch_deliveries(&d, &job, "msg")
        .await
        .expect("delivery target resolves from scope");

    let guard = seen.lock().expect("delivery ctx mutex");
    assert_eq!(guard.len(), 1);
    assert_eq!(guard[0]["channel"], json!("telegram"));
    assert_eq!(guard[0]["conv"], json!("telegram:478376391"));
    assert_eq!(guard[0]["owner"], json!("telegram:478376391"));
    assert_eq!(job.delivery_ctx, json!({}));
}

#[tokio::test]
async fn dispatch_deliveries_rejects_empty_ctx_without_scope_target() {
    let calls = Arc::new(AtomicUsize::new(0));
    let d: Vec<Arc<dyn CronDelivery>> = vec![Arc::new(CountingDelivery {
        calls: Arc::clone(&calls),
    })];
    let job = fixture("p");

    let err = dispatch_deliveries(&d, &job, "msg")
        .await
        .expect_err("missing delivery target");

    assert!(
        err.to_string().contains("cron delivery target missing"),
        "unexpected error: {err}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn builder_chain_sets_fields() {
    let store = Arc::new(crabgent_store::memory::MemoryCronStore::default());
    let kernel = dummy_kernel();
    let exec: Arc<dyn CronExecutor> = Arc::new(KernelCronExecutor::new("m"));
    let s = CronScheduler::new(store, kernel, exec)
        .with_delivery(Arc::new(NoopDelivery))
        .with_observer(Arc::new(NoopCronObserver))
        .with_tick_interval(Duration::from_millis(10))
        .with_job_timeout(Duration::from_secs(2))
        .with_job_cancel_grace(Duration::from_millis(25))
        .with_max_concurrent(2)
        .with_claim_limit(8)
        .with_stuck_recover_secs(120);
    assert_eq!(s.deliveries.len(), 1);
    assert_eq!(s.observers.len(), 1);
    assert_eq!(s.tick_interval, Duration::from_millis(10));
    assert_eq!(s.job_timeout, Duration::from_secs(2));
    assert_eq!(s.job_cancel_grace, Duration::from_millis(25));
    assert_eq!(s.max_concurrent, 2);
    assert_eq!(s.claim_limit, 8);
    assert_eq!(s.stuck_recover_secs, 120);
}

struct CancelThenPendingExecutor {
    started: Mutex<Option<oneshot::Sender<()>>>,
    cancelled: Mutex<Option<oneshot::Sender<()>>>,
}

#[async_trait]
impl CronExecutor for CancelThenPendingExecutor {
    async fn execute(&self, ctx: CronExecCtx<'_>) -> Result<CronExecResult, CronError> {
        let started = self.started.lock().expect("started mutex").take();
        if let Some(tx) = started
            && tx.send(()).is_err()
        {
            // Receiver was dropped by the test after observing enough state.
        }
        ctx.cancel.cancelled().await;
        let cancelled = self.cancelled.lock().expect("cancelled mutex").take();
        if let Some(tx) = cancelled
            && tx.send(()).is_err()
        {
            // Receiver was dropped by the test after observing enough state.
        }
        pending::<Result<CronExecResult, CronError>>().await
    }
}

fn cancel_pending_scheduler(
    started: oneshot::Sender<()>,
    cancelled: oneshot::Sender<()>,
    job_timeout: Duration,
) -> Arc<CronScheduler<crabgent_store::memory::MemoryCronStore>> {
    let store = Arc::new(crabgent_store::memory::MemoryCronStore::default());
    let executor: Arc<dyn CronExecutor> = Arc::new(CancelThenPendingExecutor {
        started: Mutex::new(Some(started)),
        cancelled: Mutex::new(Some(cancelled)),
    });
    Arc::new(
        CronScheduler::new(store, dummy_kernel(), executor)
            .with_job_timeout(job_timeout)
            .with_job_cancel_grace(Duration::from_millis(25)),
    )
}

#[tokio::test]
async fn run_executor_outer_cancel_propagates_to_job() {
    let (started_tx, started_rx) = oneshot::channel();
    let (cancelled_tx, cancelled_rx) = oneshot::channel();
    let scheduler = cancel_pending_scheduler(started_tx, cancelled_tx, Duration::from_mins(1));
    let job = fixture("p");
    let parent = CancellationToken::new();
    let child = parent.child_token();
    let handle = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        async move { run_executor(&scheduler, &job, "p", child).await }
    });

    time::timeout(Duration::from_secs(1), started_rx)
        .await
        .expect("executor should start")
        .expect("started sender should live");
    parent.cancel();
    time::timeout(Duration::from_secs(1), cancelled_rx)
        .await
        .expect("job should observe outer cancellation")
        .expect("cancelled sender should live");

    let result = time::timeout(Duration::from_secs(1), handle)
        .await
        .expect("run_executor should finish after cancel grace")
        .expect("join should succeed");
    assert_eq!(result.error.as_deref(), Some(CANCELLED_MESSAGE));
}

#[tokio::test]
async fn run_executor_timeout_cancels_before_grace_drain() {
    let (started_tx, started_rx) = oneshot::channel();
    let (cancelled_tx, cancelled_rx) = oneshot::channel();
    let scheduler = cancel_pending_scheduler(started_tx, cancelled_tx, Duration::from_millis(10));
    let job = fixture("p");
    let cancel = CancellationToken::new();

    let result = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        async move { run_executor(&scheduler, &job, "p", cancel).await }
    });

    time::timeout(Duration::from_secs(1), started_rx)
        .await
        .expect("executor should start")
        .expect("started sender should live");
    time::timeout(Duration::from_secs(1), cancelled_rx)
        .await
        .expect("timeout should cancel job before drain grace")
        .expect("cancelled sender should live");

    let result = result.await.expect("join should succeed");
    assert_eq!(result.error.as_deref(), Some("cron job timed out"));
}

#[tokio::test]
async fn scheduler_shutdown_drains_running_jobs() {
    let store = Arc::new(crabgent_store::memory::MemoryCronStore::default());
    store
        .create(&fixture("p"))
        .await
        .expect("create due cron job");
    let (started_tx, started_rx) = oneshot::channel();
    let (cancelled_tx, cancelled_rx) = oneshot::channel();
    let executor: Arc<dyn CronExecutor> = Arc::new(CancelThenPendingExecutor {
        started: Mutex::new(Some(started_tx)),
        cancelled: Mutex::new(Some(cancelled_tx)),
    });
    let scheduler = Arc::new(
        CronScheduler::new(store, dummy_kernel(), executor)
            .with_tick_interval(Duration::from_millis(10))
            .with_job_timeout(Duration::from_mins(1))
            .with_job_cancel_grace(Duration::from_millis(25)),
    );
    let cancel = CancellationToken::new();
    let handle = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        let cancel = cancel.clone();
        async move { scheduler.run(cancel).await }
    });

    time::timeout(Duration::from_secs(1), started_rx)
        .await
        .expect("job should start")
        .expect("started sender should live");
    cancel.cancel();
    time::timeout(Duration::from_secs(1), cancelled_rx)
        .await
        .expect("running job should observe scheduler shutdown")
        .expect("cancelled sender should live");

    time::timeout(Duration::from_secs(1), handle)
        .await
        .expect("scheduler should drain cancelled job")
        .expect("scheduler task should join")
        .expect("scheduler should exit cleanly");
}

struct CoopExecutor {
    record: Arc<AtomicUsize>,
}

#[async_trait]
impl CronExecutor for CoopExecutor {
    async fn execute(&self, ctx: CronExecCtx<'_>) -> Result<CronExecResult, CronError> {
        ctx.cancel.cancelled().await;
        self.record.fetch_add(1, Ordering::Relaxed);
        Ok(CronExecResult::default())
    }
}

#[tokio::test]
async fn scheduler_shutdown_releases_cooperative_executor_before_grace() {
    let store = Arc::new(crabgent_store::memory::MemoryCronStore::default());
    store
        .create(&fixture("p"))
        .await
        .expect("create due cron job");
    let record = Arc::new(AtomicUsize::new(0));
    let executor: Arc<dyn CronExecutor> = Arc::new(CoopExecutor {
        record: record.clone(),
    });
    let scheduler = Arc::new(
        CronScheduler::new(store, dummy_kernel(), executor)
            .with_tick_interval(Duration::from_millis(50))
            .with_job_timeout(Duration::from_mins(1))
            .with_job_cancel_grace(Duration::from_millis(25))
            // Generously high so any abort-fallback would be visible.
            .with_shutdown_grace(Duration::from_secs(10)),
    );
    let external_cancel = CancellationToken::new();
    let handle = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        let cancel = external_cancel.clone();
        async move { scheduler.run(cancel).await }
    });
    // Let one tick fire so the executor is parked on ctx.cancel.cancelled().
    time::sleep(Duration::from_millis(150)).await;

    let start = std::time::Instant::now();
    scheduler.shutdown().await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "cooperative drain must finish well under shutdown_grace=10s, elapsed={elapsed:?}"
    );
    assert_eq!(
        record.load(Ordering::Relaxed),
        1,
        "executor must observe ctx.cancel and complete cooperatively"
    );

    time::timeout(Duration::from_secs(1), handle)
        .await
        .expect("scheduler task should join after cooperative shutdown")
        .expect("scheduler task should not panic")
        .expect("scheduler should exit cleanly");
}
