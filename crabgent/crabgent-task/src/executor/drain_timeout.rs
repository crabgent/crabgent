//! Timeout, grace, and cancellation mechanics for draining a running task.
//!
//! Extracted from `spawn.rs` to keep that file under the 500-line cap. These
//! helpers operate purely on a pinned `Future<Output = DrainOutcome>`, a
//! `CancellationToken`, and timing budgets; they carry none of the executor's
//! `DriveCtx`/store generics, which keeps the spawn lifecycle and the drain
//! mechanics as two separate seams. `drain_tests.rs` exercises
//! `run_drain_with_timeout` directly.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crabgent_log::{debug, warn};
use crabgent_store::TaskId;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::drain::DrainOutcome;

use super::{CANCELLED_MESSAGE, TIMEOUT_MESSAGE};

pub(super) async fn run_drain_with_timeout<F>(
    task_id: &TaskId,
    cancel: CancellationToken,
    timeout: Duration,
    shutdown_grace: Duration,
    drain: F,
) -> DrainOutcome
where
    F: Future<Output = DrainOutcome>,
{
    let mut drain = Box::pin(drain);
    tokio::select! {
        biased;
        () = cancel.cancelled() => cancel_drain(
            task_id,
            shutdown_grace,
            drain.as_mut(),
        ).await,
        outcome = &mut drain => finish_drain_after_run(cancel.is_cancelled(), outcome),
        () = time::sleep(timeout) => timeout_drain(
            task_id,
            &cancel,
            shutdown_grace,
            drain.as_mut(),
        ).await,
    }
}

async fn cancel_drain<F>(
    task_id: &TaskId,
    shutdown_grace: Duration,
    drain: Pin<&mut F>,
) -> DrainOutcome
where
    F: Future<Output = DrainOutcome>,
{
    debug!(task_id = %task_id, "task executor: task cancelled; draining");
    match time::timeout(shutdown_grace, drain).await {
        Ok(outcome) => normalize_after_cancel(outcome),
        Err(_elapsed) => {
            log_drain_grace_elapsed(task_id, shutdown_grace, "shutdown cancellation");
            cancelled_outcome()
        }
    }
}

fn log_drain_grace_elapsed(task_id: &TaskId, shutdown_grace: Duration, reason: &'static str) {
    warn!(
        task_id = %task_id,
        ?shutdown_grace,
        reason,
        "task executor: drain did not finish after grace"
    );
}

fn finish_drain_after_run(cancelled: bool, outcome: DrainOutcome) -> DrainOutcome {
    if cancelled {
        return normalize_after_cancel(outcome);
    }
    outcome
}

/// A drain that reached its real terminal state during the cancel window
/// keeps it: a completed run stays Done and a cooperatively paused run
/// stays paused instead of being reclassified as cancelled (which would
/// re-execute finished work after a restart). Only a drain that was
/// genuinely killed mid-flight collapses to the cancelled outcome.
fn normalize_after_cancel(outcome: DrainOutcome) -> DrainOutcome {
    if outcome.paused || (outcome.error.is_none() && outcome.final_text.is_some()) {
        return outcome;
    }
    cancelled_outcome()
}

async fn timeout_drain<F>(
    task_id: &TaskId,
    cancel: &CancellationToken,
    shutdown_grace: Duration,
    drain: Pin<&mut F>,
) -> DrainOutcome
where
    F: Future<Output = DrainOutcome>,
{
    cancel.cancel();
    warn!(task_id = %task_id, "task executor: task timed out; cancellation requested");
    drain_after_cancel(task_id, shutdown_grace, drain, "cancellation").await;
    timeout_outcome()
}

async fn drain_after_cancel<F>(
    task_id: &TaskId,
    shutdown_grace: Duration,
    drain: Pin<&mut F>,
    reason: &'static str,
) where
    F: Future<Output = DrainOutcome>,
{
    if time::timeout(shutdown_grace, drain).await.is_err() {
        log_drain_grace_elapsed(task_id, shutdown_grace, reason);
    }
}

fn cancelled_outcome() -> DrainOutcome {
    DrainOutcome {
        final_text: None,
        error: Some(CANCELLED_MESSAGE.into()),
        paused: false,
    }
}

fn timeout_outcome() -> DrainOutcome {
    DrainOutcome {
        final_text: None,
        error: Some(TIMEOUT_MESSAGE.into()),
        paused: false,
    }
}
