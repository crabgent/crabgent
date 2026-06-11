//! Typed observer events for cron scheduler and executor progress.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::{ActivityEventSummary, ActivityTextSummary, Owner, RunId};
use crabgent_log::warn;
use crabgent_store::CronJobId;
use crabgent_store::records::CronJob;

use crate::error::CronError;

/// Receives compact progress events for cron work.
///
/// Implementations should enqueue quickly. Errors are logged and never fail
/// claiming, execution, schedule advancement, delivery, timeout, or shutdown.
#[async_trait]
pub trait CronObserver: Send + Sync {
    async fn observe(&self, event: CronActivityEvent) -> Result<(), CronError>;
}

#[derive(Debug, Default)]
pub struct NoopCronObserver;

#[async_trait]
impl CronObserver for NoopCronObserver {
    async fn observe(&self, _event: CronActivityEvent) -> Result<(), CronError> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct CronActivityEvent {
    pub observed_at: DateTime<Utc>,
    pub job: Option<CronJobActivityMeta>,
    pub kind: CronActivityKind,
}

impl CronActivityEvent {
    #[must_use]
    pub fn new(job: Option<CronJobActivityMeta>, kind: CronActivityKind) -> Self {
        Self {
            observed_at: Utc::now(),
            job,
            kind,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CronJobActivityMeta {
    pub job_id: CronJobId,
    pub name: ActivityTextSummary,
    pub owner: Option<Owner>,
    pub run_id: Option<RunId>,
    pub prompt: ActivityTextSummary,
    pub created_at: DateTime<Utc>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub last_run: Option<DateTime<Utc>>,
    pub next_run: DateTime<Utc>,
}

impl CronJobActivityMeta {
    #[must_use]
    pub fn from_job(job: &CronJob, prompt: &str, run_id: Option<RunId>) -> Self {
        Self {
            job_id: job.id.clone(),
            name: ActivityTextSummary::with_preview(&job.name),
            owner: job.scope.owner.clone(),
            run_id,
            prompt: ActivityTextSummary::redacted(prompt),
            created_at: job.created_at,
            claimed_at: job.claimed_at,
            last_run: job.last_run,
            next_run: job.next_run,
        }
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CronActivityKind {
    ClaimedBatch {
        count: usize,
        claim_limit: usize,
    },
    ClaimFailed {
        error: ActivityTextSummary,
    },
    ConcurrencyLimit {
        max_concurrent: usize,
    },
    ClaimReleased,
    ClaimReleaseFailed {
        error: ActivityTextSummary,
    },
    Started,
    PreProcessorSkipped,
    PreProcessorDelivered {
        text: ActivityTextSummary,
    },
    PreProcessorRunLlm {
        prompt: ActivityTextSummary,
    },
    PreProcessorPassthrough,
    Kernel(ActivityEventSummary),
    Completed,
    Failed {
        error: ActivityTextSummary,
    },
    Cancelled,
    TimedOut,
    ScheduleAdvanced {
        next_run: DateTime<Utc>,
        disabled: bool,
    },
    ScheduleAdvanceFailed {
        error: ActivityTextSummary,
    },
}

pub async fn notify_observers(observers: &[Arc<dyn CronObserver>], event: CronActivityEvent) {
    for observer in observers {
        notify_one(observer, event.clone()).await;
    }
}

async fn notify_one(observer: &Arc<dyn CronObserver>, event: CronActivityEvent) {
    if let Err(error) = observer.observe(event.clone()).await {
        let job_id = event.job.as_ref().map(|meta| meta.job_id.to_string());
        warn!(
            job_id = job_id.as_deref(),
            error = %error,
            "cron observer failed"
        );
    }
}
