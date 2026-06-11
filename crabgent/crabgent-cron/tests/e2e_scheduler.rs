//! End-to-end test for [`CronScheduler`].
//!
//! Drives a real `Kernel` with a stub provider, claims a due job, runs
//! it through `KernelCronExecutor`, dispatches a recording delivery,
//! and verifies that `next_run` advances. Plus a second test covering
//! the pre-processor `Deliver` short-circuit (no kernel call needed).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_core::ActivityEventSummary;
use crabgent_core::error::ProviderError;
use crabgent_core::model::ModelInfo;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse, StopReason, Usage};
use crabgent_core::{Kernel, MemoryScope, Owner, RunCtx};
use crabgent_cron::{
    CronActivityEvent, CronActivityKind, CronDelivery, CronError, CronExecCtx, CronExecResult,
    CronExecutor, CronObserver, CronPreProcessResult, CronPreProcessor, CronScheduler,
    KernelCronExecutor,
};
use crabgent_store::CronJobId;
use crabgent_store::memory::MemoryCronStore;
use crabgent_store::records::{CronJob, CronSchedule};
use crabgent_store::traits::CronStore;
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

struct StubProvider {
    text: String,
}

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Ok(LlmResponse {
            text: self.text.clone(),
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
        vec![ModelInfo::minimal("claude-haiku-4-5", "stub")]
    }
}

struct OnceDelivery {
    tx: Mutex<Option<oneshot::Sender<(CronJobId, String)>>>,
}

#[async_trait]
impl CronDelivery for OnceDelivery {
    async fn deliver(&self, job: &CronJob, message: &str) -> Result<bool, CronError> {
        let mut guard = self.tx.lock().expect("delivery mutex");
        if let Some(tx) = guard.take()
            && tx.send((job.id.clone(), message.to_owned())).is_err()
        {
            // Receiver was dropped by the test after observing enough state.
        }
        Ok(true)
    }
}

struct DeliverPre {
    text: String,
}

#[async_trait]
impl CronPreProcessor for DeliverPre {
    async fn pre_process(&self, _job: &CronJob) -> CronPreProcessResult {
        CronPreProcessResult::Deliver(self.text.clone())
    }
}

struct FailingExecutor;

#[async_trait]
impl CronExecutor for FailingExecutor {
    async fn execute(&self, _ctx: CronExecCtx<'_>) -> Result<CronExecResult, CronError> {
        // A custom executor that reports a failure via the result's `error`
        // field (no `final_text`). The scheduler must advance the schedule and
        // consume the error string rather than panicking or delivering.
        Ok(CronExecResult {
            final_text: None,
            error: Some("agent identity not configured".to_owned()),
        })
    }
}

#[derive(Default)]
struct RecordingCronObserver {
    events: Mutex<Vec<CronActivityEvent>>,
}

#[async_trait]
impl CronObserver for RecordingCronObserver {
    async fn observe(&self, event: CronActivityEvent) -> Result<(), CronError> {
        self.events.lock().expect("observer mutex").push(event);
        Ok(())
    }
}

impl RecordingCronObserver {
    fn events(&self) -> Vec<CronActivityEvent> {
        self.events.lock().expect("observer mutex").clone()
    }
}

struct FailingCronObserver;

#[async_trait]
impl CronObserver for FailingCronObserver {
    async fn observe(&self, _event: CronActivityEvent) -> Result<(), CronError> {
        Err(CronError::scheduler("observer down"))
    }
}

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
            "kernel must not be called when pre-processor Delivers".into(),
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

fn build_due_job(prompt: &str) -> CronJob {
    let now = Utc::now();
    CronJob {
        id: CronJobId::new(),
        name: "test-job".into(),
        scope: MemoryScope::for_owner(Owner::new("telegram:478376391"))
            .with_channel("telegram")
            .with_conv("telegram:478376391"),
        prompt: prompt.into(),
        schedule: CronSchedule::every(60),
        enabled: true,
        run_once: false,
        model_override: None,
        reasoning_effort_override: None,
        pre_command: None,
        delivery_ctx: json!({}),
        last_run: None,
        next_run: now,
        created_at: now,
        claimed_at: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_runs_due_job_through_kernel_and_advances_schedule() {
    let store = Arc::new(MemoryCronStore::default());
    let observer = Arc::new(RecordingCronObserver::default());
    let job = build_due_job("say hi");
    let job_id = job.id.clone();
    store.create(&job).await.expect("create job");
    let original_next = job.next_run;

    let kernel = Arc::new(
        Kernel::builder()
            .provider(StubProvider {
                text: "hello world".into(),
            })
            .policy(AllowAllPolicy)
            .build(),
    );
    let executor_observer: Arc<dyn CronObserver> = observer.clone();
    let executor: Arc<dyn CronExecutor> = Arc::new(
        KernelCronExecutor::new("claude-haiku-4-5")
            .with_observer(Arc::new(FailingCronObserver))
            .with_observer(executor_observer),
    );
    let (tx, rx) = oneshot::channel();
    let delivery: Arc<dyn CronDelivery> = Arc::new(OnceDelivery {
        tx: Mutex::new(Some(tx)),
    });
    let scheduler_observer: Arc<dyn CronObserver> = observer.clone();
    let scheduler = Arc::new(
        CronScheduler::new(Arc::clone(&store), kernel, executor)
            .with_delivery(delivery)
            .with_observer(Arc::new(FailingCronObserver))
            .with_observer(scheduler_observer)
            .with_tick_interval(Duration::from_millis(50))
            .with_job_timeout(Duration::from_secs(5))
            .with_max_concurrent(2),
    );
    let cancel = CancellationToken::new();
    let scheduler_handle = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        let cancel = cancel.clone();
        async move {
            scheduler.run(cancel).await.expect("scheduler exits clean");
        }
    });

    let (delivered_id, delivered_text) = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("delivery must fire within 5s")
        .expect("delivery sender dropped");
    assert_eq!(delivered_id, job_id);
    assert_eq!(delivered_text, "hello world");

    cancel.cancel();
    scheduler_handle.await.expect("scheduler join");

    let stored = store
        .get(&job_id)
        .await
        .expect("store get")
        .expect("job exists");
    assert!(stored.next_run > original_next);
    assert!(stored.last_run.is_some());
    assert!(stored.claimed_at.is_none());

    let events = observer.events();
    let labels = events.iter().map(cron_activity_label).collect::<Vec<_>>();
    assert_cron_activity_order(&labels);
    assert!(
        events
            .iter()
            .filter_map(|event| event.job.as_ref())
            .all(|meta| meta.prompt.preview.is_none()
                && meta.name.preview.as_deref() == Some("test-job"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_advances_schedule_when_executor_reports_failure() {
    let store = Arc::new(MemoryCronStore::default());
    let job = build_due_job("will fail");
    let job_id = job.id.clone();
    store.create(&job).await.expect("create job");
    let original_next = job.next_run;

    // The executor never touches the kernel, so a panic-on-call provider proves
    // the failure path does not reach the LLM.
    let kernel = Arc::new(
        Kernel::builder()
            .provider(PanicProvider)
            .policy(AllowAllPolicy)
            .build(),
    );
    let executor: Arc<dyn CronExecutor> = Arc::new(FailingExecutor);
    let scheduler = Arc::new(
        CronScheduler::new(Arc::clone(&store), kernel, executor)
            .with_tick_interval(Duration::from_millis(50)),
    );
    let cancel = CancellationToken::new();
    let scheduler_handle = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        let cancel = cancel.clone();
        async move {
            scheduler.run(cancel).await.expect("scheduler exits clean");
        }
    });

    // No delivery fires for a failed run, so poll the store until the schedule
    // advances. This exercises `advance_schedule`'s error-consumption branch.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let stored = loop {
        let current = store
            .get(&job_id)
            .await
            .expect("store get")
            .expect("job exists");
        if current.last_run.is_some() && current.claimed_at.is_none() {
            break current;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "schedule did not advance after executor failure within 5s"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    cancel.cancel();
    scheduler_handle.await.expect("scheduler join");

    assert!(stored.next_run > original_next);
    assert!(stored.last_run.is_some());
    assert!(stored.claimed_at.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_pre_processor_deliver_skips_kernel() {
    let store = Arc::new(MemoryCronStore::default());
    let job = build_due_job("p");
    let job_id = job.id.clone();
    store.create(&job).await.expect("create job");

    // Provider that would panic if called, proving Deliver short-circuits.
    let kernel = Arc::new(
        Kernel::builder()
            .provider(PanicProvider)
            .policy(AllowAllPolicy)
            .build(),
    );
    let executor: Arc<dyn CronExecutor> = Arc::new(KernelCronExecutor::new("m"));
    let (tx, rx) = oneshot::channel();
    let delivery: Arc<dyn CronDelivery> = Arc::new(OnceDelivery {
        tx: Mutex::new(Some(tx)),
    });
    let pre: Arc<dyn CronPreProcessor> = Arc::new(DeliverPre {
        text: "preprocessed".into(),
    });
    let scheduler = Arc::new(
        CronScheduler::new(Arc::clone(&store), kernel, executor)
            .with_delivery(delivery)
            .with_pre_processor(pre)
            .with_tick_interval(Duration::from_millis(50)),
    );
    let cancel = CancellationToken::new();
    let scheduler_handle = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        let cancel = cancel.clone();
        async move {
            scheduler.run(cancel).await.expect("scheduler exits clean");
        }
    });

    let (delivered_id, delivered_text) = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("delivery must fire within 5s")
        .expect("delivery sender dropped");
    assert_eq!(delivered_id, job_id);
    assert_eq!(delivered_text, "preprocessed");

    cancel.cancel();
    scheduler_handle.await.expect("scheduler join");
}

const fn cron_activity_label(event: &CronActivityEvent) -> &'static str {
    match &event.kind {
        CronActivityKind::ClaimedBatch { .. } => "claimed_batch",
        CronActivityKind::ClaimFailed { .. } => "claim_failed",
        CronActivityKind::ConcurrencyLimit { .. } => "concurrency_limit",
        CronActivityKind::ClaimReleased => "claim_released",
        CronActivityKind::ClaimReleaseFailed { .. } => "claim_release_failed",
        CronActivityKind::Started => "started",
        CronActivityKind::PreProcessorSkipped => "pre_skipped",
        CronActivityKind::PreProcessorDelivered { .. } => "pre_delivered",
        CronActivityKind::PreProcessorRunLlm { .. } => "pre_run_llm",
        CronActivityKind::PreProcessorPassthrough => "pre_passthrough",
        CronActivityKind::Kernel(ActivityEventSummary::OutputDelta(_)) => "output_delta",
        CronActivityKind::Kernel(ActivityEventSummary::ReasoningDelta(_)) => "reasoning_delta",
        CronActivityKind::Kernel(ActivityEventSummary::ToolCallStarted(_)) => "tool_started",
        CronActivityKind::Kernel(ActivityEventSummary::ToolCallCompleted(_)) => "tool_completed",
        CronActivityKind::Kernel(ActivityEventSummary::Notification(_)) => "notification",
        CronActivityKind::Kernel(ActivityEventSummary::ServerToolResult(_)) => "server_tool",
        CronActivityKind::Kernel(ActivityEventSummary::AttemptFailed(_)) => "attempt_failed",
        CronActivityKind::Kernel(ActivityEventSummary::Final(_)) => "final",
        CronActivityKind::Completed => "completed",
        CronActivityKind::Failed { .. } => "failed",
        CronActivityKind::Cancelled => "cancelled",
        CronActivityKind::TimedOut => "timed_out",
        CronActivityKind::ScheduleAdvanced { .. } => "schedule_advanced",
        CronActivityKind::ScheduleAdvanceFailed { .. } => "schedule_advance_failed",
        _ => "unknown",
    }
}

fn assert_cron_activity_order(labels: &[&'static str]) {
    let claimed = cron_activity_index(labels, "claimed_batch");
    let started = cron_activity_index(labels, "started");
    let pre_passthrough = cron_activity_index(labels, "pre_passthrough");
    let output_delta = cron_activity_index(labels, "output_delta");
    let final_event = cron_activity_index(labels, "final");
    let schedule_advanced = cron_activity_index(labels, "schedule_advanced");
    let completed = cron_activity_index(labels, "completed");

    assert!(claimed < started);
    assert!(started < pre_passthrough);
    assert!(pre_passthrough < output_delta);
    assert!(output_delta < final_event);
    assert!(final_event < schedule_advanced);
    assert!(schedule_advanced < completed);
}

fn cron_activity_index(labels: &[&'static str], needle: &'static str) -> usize {
    labels
        .iter()
        .position(|label| *label == needle)
        .expect("cron activity event should be present")
}
