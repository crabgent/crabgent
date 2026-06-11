use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use crabgent_core::{ActivityTextSummary, MemoryScope};
use crabgent_log::{debug, error, info, warn};
use crabgent_store::records::CronJob;
use crabgent_store::traits::CronStore;
use serde_json::{Map, Value};
use tokio::task::JoinSet;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::delivery::CronDelivery;
use crate::error::CronError;
use crate::executor::{CronExecCtx, CronExecResult};
use crate::observer::{CronActivityEvent, CronActivityKind, CronJobActivityMeta, notify_observers};
use crate::pre_processor::{CronPreProcessResult, CronPreProcessor};
use crate::schedule;

use super::CronScheduler;

const FALLBACK_RETRY_SECS: i64 = 60;
pub(super) const CANCELLED_MESSAGE: &str = "cron job cancelled";
pub(super) const TIMEOUT_MESSAGE: &str = "cron job timed out";

pub(super) fn log_recovered_claims(count: usize) {
    if count > 0 {
        info!(count, "cron: recovered stuck claims");
    }
}

pub(super) async fn drain_join_set(jobs: &mut JoinSet<()>, grace: Duration) -> bool {
    time::timeout(grace, async { while jobs.join_next().await.is_some() {} })
        .await
        .is_ok()
}

pub(super) fn log_scheduler_drain_start(job_count: usize) {
    info!(
        "cron scheduler stopping; draining {} running jobs",
        job_count
    );
}

pub(super) async fn abort_jobs_after_grace(jobs: &mut JoinSet<()>, grace: Duration) {
    warn!(
        ?grace,
        "cron scheduler shutdown grace elapsed; aborting jobs"
    );
    jobs.abort_all();
    drain_aborted_jobs(jobs).await;
}

async fn drain_aborted_jobs(jobs: &mut JoinSet<()>) {
    while jobs.join_next().await.is_some() {}
}

pub(super) fn drain_finished(jobs: &mut JoinSet<()>) {
    while let Some(result) = jobs.try_join_next() {
        if let Err(e) = result {
            error!(error = ?e, "cron: worker panicked");
        }
    }
}

pub(super) async fn drive_one_job<S>(
    scheduler: Arc<CronScheduler<S>>,
    job: CronJob,
    permit: tokio::sync::OwnedSemaphorePermit,
    cancel: CancellationToken,
) where
    S: CronStore + 'static,
{
    observe_job(&scheduler, &job, CronActivityKind::Started).await;
    let outcome = run_pre_processors(&scheduler.pre_processors, &job).await;
    observe_pre_process_outcome(&scheduler, &job, &outcome).await;
    let skipped = matches!(outcome, CronPreProcessResult::Skip);
    let mut result = match outcome {
        CronPreProcessResult::Skip => CronExecResult::default(),
        CronPreProcessResult::Deliver(text) => CronExecResult {
            final_text: Some(text),
            error: None,
        },
        CronPreProcessResult::RunLlm(prompt) => {
            run_executor(&scheduler, &job, &prompt, cancel).await
        }
        CronPreProcessResult::Passthrough => {
            run_executor(&scheduler, &job, &job.prompt, cancel).await
        }
    };
    if !skipped
        && let Some(message) = result.final_text.as_deref()
        && let Err(error) = dispatch_deliveries(&scheduler.deliveries, &job, message).await
    {
        result = executor_error_result(error.to_string());
    }
    advance_schedule(&scheduler, &job, &result).await;
    observe_job(&scheduler, &job, terminal_activity_kind(&result)).await;
    drop(permit);
}

pub(super) async fn run_pre_processors(
    pre_processors: &[Arc<dyn CronPreProcessor>],
    job: &CronJob,
) -> CronPreProcessResult {
    for p in pre_processors {
        let outcome = p.pre_process(job).await;
        if !matches!(outcome, CronPreProcessResult::Passthrough) {
            return outcome;
        }
    }
    CronPreProcessResult::Passthrough
}

pub(super) async fn run_executor<S>(
    scheduler: &CronScheduler<S>,
    job: &CronJob,
    prompt: &str,
    cancel: CancellationToken,
) -> CronExecResult
where
    S: CronStore,
{
    let ctx = CronExecCtx {
        job,
        kernel: Arc::clone(&scheduler.kernel),
        prompt,
        cancel: cancel.clone(),
    };
    let mut fut = Box::pin(scheduler.executor.execute(ctx));
    tokio::select! {
        result = &mut fut => finish_executor_result(job, result),
        () = time::sleep(scheduler.job_timeout) => timeout_executor(
            job,
            &cancel,
            scheduler.job_cancel_grace,
            fut.as_mut(),
        ).await,
        () = cancel.cancelled() => cancel_executor(
            job,
            scheduler.job_cancel_grace,
            fut.as_mut(),
        ).await,
    }
}

fn finish_executor_result(
    job: &CronJob,
    result: Result<CronExecResult, CronError>,
) -> CronExecResult {
    match result {
        Ok(result) => result,
        Err(e) => {
            error!(job = %job.name, error = %e, "cron: executor failed");
            executor_error_result(e.to_string())
        }
    }
}

async fn timeout_executor<F>(
    job: &CronJob,
    cancel: &CancellationToken,
    grace: Duration,
    fut: Pin<&mut F>,
) -> CronExecResult
where
    F: Future<Output = Result<CronExecResult, CronError>>,
{
    cancel.cancel();
    warn!(job = %job.name, "cron: executor timed out; cancellation requested");
    drain_executor_after_cancel(job, grace, fut, "cancellation").await;
    executor_error_result(TIMEOUT_MESSAGE.into())
}

async fn cancel_executor<F>(job: &CronJob, grace: Duration, fut: Pin<&mut F>) -> CronExecResult
where
    F: Future<Output = Result<CronExecResult, CronError>>,
{
    debug!(job = %job.name, "cron: executor cancelled; draining");
    drain_executor_after_cancel(job, grace, fut, "shutdown cancellation").await;
    executor_error_result(CANCELLED_MESSAGE.into())
}

async fn drain_executor_after_cancel<F>(
    job: &CronJob,
    grace: Duration,
    fut: Pin<&mut F>,
    reason: &'static str,
) where
    F: Future<Output = Result<CronExecResult, CronError>>,
{
    if time::timeout(grace, fut).await.is_err() {
        warn!(
            job = %job.name,
            ?grace,
            reason,
            "cron: executor did not finish after grace"
        );
    }
}

const fn executor_error_result(error: String) -> CronExecResult {
    CronExecResult {
        final_text: None,
        error: Some(error),
    }
}

pub(super) async fn dispatch_deliveries(
    deliveries: &[Arc<dyn CronDelivery>],
    job: &CronJob,
    message: &str,
) -> Result<(), CronError> {
    if deliveries.is_empty() {
        return Ok(());
    }
    let delivery_job = effective_delivery_job(job)?;
    for d in deliveries {
        log_delivery_result(job, d.deliver(delivery_job.as_ref(), message).await);
    }
    Ok(())
}

fn effective_delivery_job(job: &CronJob) -> Result<Cow<'_, CronJob>, CronError> {
    match job.delivery_ctx.as_object() {
        Some(ctx) if ctx.is_empty() => {
            let mut job = job.clone();
            job.delivery_ctx = delivery_ctx_from_scope(&job.scope)?;
            Ok(Cow::Owned(job))
        }
        Some(_) => Ok(Cow::Borrowed(job)),
        None => Err(CronError::delivery(
            "cron delivery_ctx must be a JSON object",
        )),
    }
}

fn delivery_ctx_from_scope(scope: &MemoryScope) -> Result<Value, CronError> {
    let conv = scope
        .conv
        .as_deref()
        .or_else(|| {
            scope
                .owner
                .as_ref()
                .map(crabgent_store::Owner::as_str)
                .filter(is_prefixed_owner)
        })
        .ok_or_else(|| {
            CronError::delivery("cron delivery target missing scope.conv or prefixed scope.owner")
        })?;
    let prefix = channel_prefix(conv).ok_or_else(|| {
        CronError::delivery("cron delivery target scope.conv has no channel prefix")
    })?;
    let channel = scope.channel.as_deref().unwrap_or(prefix);
    if prefix != channel {
        return Err(CronError::delivery(
            "cron delivery target mismatch: scope.channel does not match scope.conv prefix",
        ));
    }

    let mut ctx = Map::new();
    ctx.insert("channel".to_owned(), Value::String(channel.to_owned()));
    ctx.insert("conv".to_owned(), Value::String(conv.to_owned()));
    if let Some(owner) = scope.owner.as_ref() {
        ctx.insert("owner".to_owned(), Value::String(owner.as_str().to_owned()));
    }
    Ok(Value::Object(ctx))
}

fn is_prefixed_owner(owner: &&str) -> bool {
    channel_prefix(owner).is_some()
}

fn channel_prefix(conv: &str) -> Option<&str> {
    let (channel, rest) = conv.split_once(':')?;
    if channel.is_empty() || rest.is_empty() {
        None
    } else {
        Some(channel)
    }
}

fn log_delivery_result(job: &CronJob, result: Result<bool, CronError>) {
    match result {
        Ok(delivered) => log_delivery_outcome(job, delivered),
        Err(error) => log_delivery_error(job, &error),
    }
}

fn log_delivery_outcome(job: &CronJob, delivered: bool) {
    if !delivered {
        warn!(job = %job.name, "cron: delivery rejected message");
    }
}

fn log_delivery_error(job: &CronJob, error: &CronError) {
    error!(job = %job.name, error = %error, "cron: delivery failed");
}

/// Surface a captured failure before it is discarded. Executor-internal
/// failures (timeout, cancel, kernel error) are already logged at the capture
/// site, but a custom `CronExecutor` may return a failed result with an `error`
/// string and no log of its own (e.g. an unconfigured agent identity). This is
/// the single consumption point for the field and keeps such failures debuggable.
fn log_reported_failure(job: &CronJob, result: &CronExecResult) {
    if let Some(message) = result.error.as_deref() {
        warn!(job = %job.name, error = message, "cron: job run reported failure");
    }
}

async fn advance_schedule<S: CronStore>(
    scheduler: &CronScheduler<S>,
    job: &CronJob,
    result: &CronExecResult,
) {
    log_reported_failure(job, result);
    let now = Utc::now();
    let next = schedule::next_run(&job.schedule, now)
        .unwrap_or_else(|| now + chrono::Duration::seconds(FALLBACK_RETRY_SECS));
    let disable = job.run_once && result.is_success();
    match scheduler
        .store
        .finish_claim(&job.id, now, next, disable)
        .await
    {
        Ok(()) => {
            observe_job(
                scheduler,
                job,
                CronActivityKind::ScheduleAdvanced {
                    next_run: next,
                    disabled: disable,
                },
            )
            .await;
        }
        Err(e) => {
            error!(job = %job.name, error = %e, "cron: finish_claim failed");
            observe_job(
                scheduler,
                job,
                CronActivityKind::ScheduleAdvanceFailed {
                    error: ActivityTextSummary::redacted(&e.to_string()),
                },
            )
            .await;
        }
    }
}

async fn observe_pre_process_outcome<S>(
    scheduler: &CronScheduler<S>,
    job: &CronJob,
    outcome: &CronPreProcessResult,
) where
    S: CronStore,
{
    let kind = match outcome {
        CronPreProcessResult::Skip => CronActivityKind::PreProcessorSkipped,
        CronPreProcessResult::Deliver(text) => CronActivityKind::PreProcessorDelivered {
            text: ActivityTextSummary::redacted(text),
        },
        CronPreProcessResult::RunLlm(prompt) => CronActivityKind::PreProcessorRunLlm {
            prompt: ActivityTextSummary::redacted(prompt),
        },
        CronPreProcessResult::Passthrough => CronActivityKind::PreProcessorPassthrough,
    };
    observe_job(scheduler, job, kind).await;
}

async fn observe_job<S>(scheduler: &CronScheduler<S>, job: &CronJob, kind: CronActivityKind)
where
    S: CronStore,
{
    notify_observers(
        &scheduler.observers,
        CronActivityEvent::new(
            Some(CronJobActivityMeta::from_job(job, &job.prompt, None)),
            kind,
        ),
    )
    .await;
}

fn terminal_activity_kind(result: &CronExecResult) -> CronActivityKind {
    match result.error.as_deref() {
        None => CronActivityKind::Completed,
        Some(CANCELLED_MESSAGE) => CronActivityKind::Cancelled,
        Some(TIMEOUT_MESSAGE) => CronActivityKind::TimedOut,
        Some(error) => CronActivityKind::Failed {
            error: ActivityTextSummary::redacted(error),
        },
    }
}
