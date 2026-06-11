//! [`CronScheduler`]: claim-based tick loop that runs cron jobs against a
//! kernel and routes results through deliveries.
//!
//! The scheduler is generic over any [`CronStore`]. Each tick:
//! 1. Claim up to `claim_limit` due jobs atomically.
//! 2. For each job: acquire a semaphore permit, spawn a tracked worker
//!    that runs pre-processors, executes the job (with a timeout), then
//!    dispatches deliveries and advances the schedule.
//! 3. If the semaphore is full, release the claim without advancing and
//!    let the next tick re-claim.
//!
//! Stuck-claim recovery runs once at startup and can be re-triggered by
//! callers that want a periodic sweep.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use crabgent_core::Kernel;
use crabgent_log::{error, warn};
use crabgent_store::records::CronJob;
use crabgent_store::traits::CronStore;
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinSet;
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::delivery::CronDelivery;
use crate::error::CronError;
use crate::executor::CronExecutor;
use crate::observer::{
    CronActivityEvent, CronActivityKind, CronJobActivityMeta, CronObserver, notify_observers,
};
use crate::pre_processor::CronPreProcessor;
use worker::{
    abort_jobs_after_grace, drain_finished, drain_join_set, drive_one_job, log_recovered_claims,
    log_scheduler_drain_start,
};

const DEFAULT_TICK_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_JOB_TIMEOUT: Duration = Duration::from_mins(5);
const DEFAULT_JOB_CANCEL_GRACE: Duration = Duration::from_secs(5);
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const DEFAULT_MAX_CONCURRENT: usize = 4;
const DEFAULT_CLAIM_LIMIT: usize = 100;
const DEFAULT_STUCK_RECOVER_SECS: i64 = 600;

mod worker;

/// Scheduler that drives cron jobs from a [`CronStore`].
pub struct CronScheduler<S: CronStore> {
    store: Arc<S>,
    kernel: Arc<Kernel>,
    executor: Arc<dyn CronExecutor>,
    deliveries: Vec<Arc<dyn CronDelivery>>,
    pre_processors: Vec<Arc<dyn CronPreProcessor>>,
    observers: Vec<Arc<dyn CronObserver>>,
    tick_interval: Duration,
    job_timeout: Duration,
    job_cancel_grace: Duration,
    max_concurrent: usize,
    claim_limit: usize,
    stuck_recover_secs: i64,
    cancel: CancellationToken,
    shutdown_grace: Duration,
    shutdown_done: Arc<Notify>,
}

struct SchedulerRunState {
    semaphore: Arc<Semaphore>,
    jobs: JoinSet<()>,
    interval: time::Interval,
}

impl<S: CronStore + 'static> CronScheduler<S> {
    /// Construct a new scheduler bound to `store`, `kernel`, and `executor`.
    pub fn new(store: Arc<S>, kernel: Arc<Kernel>, executor: Arc<dyn CronExecutor>) -> Self {
        Self {
            store,
            kernel,
            executor,
            deliveries: Vec::new(),
            pre_processors: Vec::new(),
            observers: Vec::new(),
            tick_interval: DEFAULT_TICK_INTERVAL,
            job_timeout: DEFAULT_JOB_TIMEOUT,
            job_cancel_grace: DEFAULT_JOB_CANCEL_GRACE,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            claim_limit: DEFAULT_CLAIM_LIMIT,
            stuck_recover_secs: DEFAULT_STUCK_RECOVER_SECS,
            cancel: CancellationToken::new(),
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
            shutdown_done: Arc::new(Notify::new()),
        }
    }

    /// Derive the scheduler-owned cancellation token from `parent`. Use
    /// this to chain into the kernel's `Kernel::shutdown_token()` so a
    /// kernel-wide shutdown signal stops the scheduler as well.
    /// Independent of the per-tick `cancel` parameter of [`Self::run`].
    #[must_use]
    pub fn with_cancel(mut self, parent: &CancellationToken) -> Self {
        self.cancel = parent.child_token();
        self
    }

    /// Drain deadline for [`Self::shutdown`] (default 5s). After
    /// cancellation, in-flight job tasks are awaited up to this grace.
    /// On overshoot, the remaining tasks are `abort_all`-ed. Distinct
    /// from [`Self::with_job_cancel_grace`], which is the per-job hard
    /// timeout grace.
    #[must_use]
    pub const fn with_shutdown_grace(mut self, d: Duration) -> Self {
        self.shutdown_grace = d;
        self
    }

    /// Register a delivery channel. Multiple deliveries fire sequentially.
    #[must_use]
    pub fn with_delivery(mut self, d: Arc<dyn CronDelivery>) -> Self {
        self.deliveries.push(d);
        self
    }

    /// Register a pre-processor. The first non-`Passthrough` outcome wins.
    #[must_use]
    pub fn with_pre_processor(mut self, p: Arc<dyn CronPreProcessor>) -> Self {
        self.pre_processors.push(p);
        self
    }

    /// Register a compact progress observer for scheduler lifecycle events.
    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn CronObserver>) -> Self {
        self.observers.push(observer);
        self
    }

    /// Tick interval (default 30s).
    #[must_use]
    pub const fn with_tick_interval(mut self, d: Duration) -> Self {
        self.tick_interval = d;
        self
    }

    /// Per-job timeout (default 300s). Hard wall on `executor.execute()`.
    #[must_use]
    pub const fn with_job_timeout(mut self, d: Duration) -> Self {
        self.job_timeout = d;
        self
    }

    /// Grace period after timeout or shutdown cancellation before the job
    /// future is dropped (default 5s).
    #[must_use]
    pub const fn with_job_cancel_grace(mut self, d: Duration) -> Self {
        self.job_cancel_grace = d;
        self
    }

    /// Maximum concurrent jobs (default 4). Excess claims are released.
    #[must_use]
    pub const fn with_max_concurrent(mut self, n: usize) -> Self {
        self.max_concurrent = n;
        self
    }

    /// Maximum jobs claimed per tick (default 100).
    #[must_use]
    pub const fn with_claim_limit(mut self, n: usize) -> Self {
        self.claim_limit = n;
        self
    }

    /// Threshold past which a `claimed_at` is considered stuck (default 600s).
    #[must_use]
    pub const fn with_stuck_recover_secs(mut self, s: i64) -> Self {
        self.stuck_recover_secs = s;
        self
    }

    /// Run the scheduler until `cancel` fires or an unrecoverable error
    /// surfaces (e.g. `claim_due` repeatedly fails). Spawns one task per
    /// dispatched job; awaits all of them on shutdown.
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) -> Result<(), CronError> {
        self.recover_stuck_at_startup().await;
        let mut state = self.run_state();
        state.interval.tick().await;
        self.run_loop(cancel, &mut state).await;
        self.drain_jobs(&mut state.jobs).await;
        self.shutdown_done.notify_waiters();
        Ok(())
    }

    fn run_state(&self) -> SchedulerRunState {
        let mut interval = time::interval(self.tick_interval);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        SchedulerRunState {
            semaphore: Arc::new(Semaphore::new(self.max_concurrent)),
            jobs: JoinSet::new(),
            interval,
        }
    }

    async fn run_loop(self: &Arc<Self>, cancel: CancellationToken, state: &mut SchedulerRunState) {
        loop {
            if self.run_loop_step(&cancel, state).await {
                break;
            }
        }
    }

    async fn run_loop_step(
        self: &Arc<Self>,
        cancel: &CancellationToken,
        state: &mut SchedulerRunState,
    ) -> bool {
        tokio::select! {
            _ = state.interval.tick() => {
                self.handle_tick(state).await;
                false
            }
            () = cancel.cancelled() => {
                // Bridge external cancel into the scheduler-owned
                // token so worker child tokens (rooted in self.cancel)
                // see the cancel cooperatively.
                self.cancel.cancel();
                true
            }
            () = self.cancel.cancelled() => true,
        }
    }

    async fn handle_tick(self: &Arc<Self>, state: &mut SchedulerRunState) {
        drain_finished(&mut state.jobs);
        self.tick_once(&state.semaphore, &mut state.jobs).await;
    }

    async fn drain_jobs(&self, jobs: &mut JoinSet<()>) {
        log_scheduler_drain_start(jobs.len());
        let grace = self.shutdown_grace;
        if drain_join_set(jobs, grace).await {
            return;
        }
        abort_jobs_after_grace(jobs, grace).await;
    }

    /// Trigger cooperative scheduler shutdown.
    ///
    /// Cancels the scheduler-owned `CancellationToken`. The active
    /// [`Self::run`] loop observes the cancel, drains its `JoinSet`
    /// inside `self.shutdown_grace`, falls back to `abort_all` on
    /// timeout, then notifies completion.
    ///
    /// This call waits up to `2 * shutdown_grace` before returning. The
    /// upper bound covers degenerate paths where the run loop never
    /// started, panicked before notify, or is wedged outside the drain
    /// path.
    ///
    /// The `Notify` future is subscribed BEFORE the cancel fires, so
    /// the wake-up signal cannot be missed in the race between cancel
    /// and the subscription point.
    pub async fn shutdown(&self) {
        let notified = self.shutdown_done.notified();
        tokio::pin!(notified);
        self.cancel.cancel();
        let hard_cap = self.shutdown_grace.saturating_mul(2);
        // Hard upper bound: ignore the timeout result. Notify completes
        // happy-path; timeout covers the degenerate "run never started"
        // case (panic before notify, missing spawn).
        if time::timeout(hard_cap, notified).await.is_err() {
            warn!(
                ?hard_cap,
                "cron scheduler shutdown hit hard upper bound; returning without rendezvous"
            );
        }
    }

    async fn recover_stuck_at_startup(&self) {
        match self.store.recover_stuck(self.stuck_recover_secs).await {
            Ok(ids) => log_recovered_claims(ids.len()),
            Err(e) => warn!(error = %e, "cron: failed to recover stuck claims"),
        }
    }

    async fn tick_once(self: &Arc<Self>, semaphore: &Arc<Semaphore>, jobs: &mut JoinSet<()>) {
        let Some(claimed) = self.claim_due_jobs().await else {
            return;
        };
        for job in claimed {
            self.spawn_or_release_job(semaphore, jobs, job).await;
        }
    }

    async fn claim_due_jobs(&self) -> Option<Vec<CronJob>> {
        match self.store.claim_due(Utc::now(), self.claim_limit).await {
            Ok(claimed) => {
                notify_observers(
                    &self.observers,
                    CronActivityEvent::new(
                        None,
                        CronActivityKind::ClaimedBatch {
                            count: claimed.len(),
                            claim_limit: self.claim_limit,
                        },
                    ),
                )
                .await;
                Some(claimed)
            }
            Err(e) => {
                error!(error = %e, "cron: claim_due failed");
                notify_observers(
                    &self.observers,
                    CronActivityEvent::new(
                        None,
                        CronActivityKind::ClaimFailed {
                            error: crabgent_core::ActivityTextSummary::redacted(&e.to_string()),
                        },
                    ),
                )
                .await;
                None
            }
        }
    }

    async fn spawn_or_release_job(
        self: &Arc<Self>,
        semaphore: &Arc<Semaphore>,
        jobs: &mut JoinSet<()>,
        job: CronJob,
    ) {
        let Ok(permit) = Arc::clone(semaphore).try_acquire_owned() else {
            warn!(job = %job.name, "cron: at concurrency limit, releasing claim");
            let meta = CronJobActivityMeta::from_job(&job, &job.prompt, None);
            notify_observers(
                &self.observers,
                CronActivityEvent::new(
                    Some(meta),
                    CronActivityKind::ConcurrencyLimit {
                        max_concurrent: self.max_concurrent,
                    },
                ),
            )
            .await;
            self.release_unclaimed(&job).await;
            return;
        };
        self.spawn_job(jobs, job, permit);
    }

    fn spawn_job(
        self: &Arc<Self>,
        jobs: &mut JoinSet<()>,
        job: CronJob,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) {
        let me = Arc::clone(self);
        // Worker tokens derive from the scheduler-owned root so
        // shutdown() (which cancels self.cancel) propagates to every
        // in-flight executor. External run(cancel) is bridged into
        // self.cancel in the run-loop select-arm.
        let job_cancel = self.cancel.child_token();
        jobs.spawn(async move {
            drive_one_job(me, job, permit, job_cancel).await;
        });
    }

    async fn release_unclaimed(&self, job: &CronJob) {
        let meta = CronJobActivityMeta::from_job(job, &job.prompt, None);
        match self.store.release_claim_only(&job.id).await {
            Ok(()) => {
                notify_observers(
                    &self.observers,
                    CronActivityEvent::new(Some(meta), CronActivityKind::ClaimReleased),
                )
                .await;
            }
            Err(e) => {
                error!(job = %job.name, error = %e, "cron: failed to release unclaimed job");
                notify_observers(
                    &self.observers,
                    CronActivityEvent::new(
                        Some(meta),
                        CronActivityKind::ClaimReleaseFailed {
                            error: crabgent_core::ActivityTextSummary::redacted(&e.to_string()),
                        },
                    ),
                )
                .await;
            }
        }
    }
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod scheduler_tests;
