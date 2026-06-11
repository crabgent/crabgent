use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crabgent_core::error::KernelError;
use crabgent_core::hook::CancelReason;
use crabgent_core::run_id::RunId;
use crabgent_log::{debug, error, warn};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinError, JoinSet};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::error::ChannelError;

pub const DEFAULT_MAX_CONCURRENT_RUNS: usize = 16;
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Key identifying a unique conversation: `(channel_name, conv_owner_string)`.
///
/// Private, callers use the `try_claim_conv` / `release_conv` methods.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConvKey(pub String, pub String);

pub struct ConvEntry {
    pub(crate) run_id: RunId,
    pub(crate) cancel: CancellationToken,
    /// Shared write-once attribution cell. `cancel_conv` writes
    /// `CancelReason::StopPattern` here before firing `cancel`, so the
    /// caller's `RunCtx.cancel_reason` (threaded through the kernel via
    /// `RunRequest.cancel_reason`) observes the cause once the run-loop
    /// short-circuits.
    pub(crate) cancel_reason: Arc<OnceLock<CancelReason>>,
}

/// Result of attempting to claim a conversation slot for a new run.
pub enum ClaimResult {
    /// No active run exists for this conv; the caller should spawn a new one.
    Spawned {
        cancel: CancellationToken,
        /// Same `cancel_reason` cell stored on the matching `ConvEntry`.
        /// The caller installs it on `RunRequest.cancel_reason` so the
        /// kernel run-loop threads it into `RunCtx.cancel_reason`.
        cancel_reason: Arc<OnceLock<CancelReason>>,
    },
    /// An active run already owns this conv; inject into it instead.
    Existing(RunId),
}

struct RunTaskOutcome {
    run_id: RunId,
    result: Result<(), KernelError>,
}

pub struct InboxLifecycle {
    max_concurrent: usize,
    semaphore: Arc<Semaphore>,
    cancel: CancellationToken,
    tasks: Mutex<JoinSet<RunTaskOutcome>>,
    shutdown_grace: Duration,
    /// Tracks which `RunId` currently owns each active `(channel, conv)` pair.
    ///
    /// Invariant: an entry is present from the moment `try_claim_conv` returns
    /// `ClaimResult::Spawned` until `release_conv` is called (on success,
    /// error, or cancellation). The caller who receives `Spawned` is responsible
    /// for calling `release_conv` at all exit points.
    active_runs: Mutex<HashMap<ConvKey, ConvEntry>>,
}

impl InboxLifecycle {
    pub(crate) fn new(max_concurrent: usize) -> Self {
        Self::new_with_grace(max_concurrent, DEFAULT_SHUTDOWN_GRACE)
    }

    pub(crate) fn new_with_grace(max_concurrent: usize, shutdown_grace: Duration) -> Self {
        let max_concurrent = max_concurrent.max(1);
        Self {
            max_concurrent,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            cancel: CancellationToken::new(),
            tasks: Mutex::new(JoinSet::new()),
            shutdown_grace,
            active_runs: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) const fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    pub(crate) const fn shutdown_grace(&self) -> Duration {
        self.shutdown_grace
    }

    pub(crate) fn child_token(&self) -> CancellationToken {
        self.cancel.child_token()
    }

    pub(crate) fn is_shutdown(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Atomically check whether `conv_key` already has an active run.
    ///
    /// - If no active run exists, insert `new_run_id` and return
    ///   [`ClaimResult::Spawned`]. The caller MUST call [`Self::release_conv`]
    ///   when the run finishes.
    /// - If an active run exists, return [`ClaimResult::Existing`] with that
    ///   run's id so the caller can inject the new event into it.
    pub(crate) async fn try_claim_conv(&self, conv_key: ConvKey, new_run_id: RunId) -> ClaimResult {
        let mut map = self.active_runs.lock().await;
        if let Some(existing) = map.get(&conv_key) {
            ClaimResult::Existing(existing.run_id.clone())
        } else {
            let cancel = self.child_token();
            let cancel_reason: Arc<OnceLock<CancelReason>> = Arc::new(OnceLock::new());
            map.insert(
                conv_key,
                ConvEntry {
                    run_id: new_run_id,
                    cancel: cancel.clone(),
                    cancel_reason: Arc::clone(&cancel_reason),
                },
            );
            ClaimResult::Spawned {
                cancel,
                cancel_reason,
            }
        }
    }

    /// Remove the active-run mapping for `conv_key` if `run_id` still owns it.
    /// Call this once at every exit point of a spawned run.
    pub(crate) async fn release_conv(&self, conv_key: &ConvKey, run_id: &RunId) {
        let mut map = self.active_runs.lock().await;
        if map
            .get(conv_key)
            .is_some_and(|entry| &entry.run_id == run_id)
        {
            map.remove(conv_key);
        }
    }

    pub(crate) async fn cancel_conv(&self, conv_key: &ConvKey) -> bool {
        let removed = self.active_runs.lock().await.remove(conv_key);
        match removed {
            Some(entry) => {
                // Attribute the cancel BEFORE firing the token so the
                // run-loop's `Outcome::Cancelled` observers see
                // `CancelReason::StopPattern` instead of an empty cell.
                // A pre-existing value means another observer (hook) won
                // the race; keep their attribution and log at debug.
                if let Err(rejected) = entry.cancel_reason.set(CancelReason::StopPattern) {
                    debug!(
                        ?rejected,
                        run_id = %entry.run_id,
                        "stop-pattern cancel observed prior cancel_reason attribution",
                    );
                }
                entry.cancel.cancel();
                true
            }
            None => false,
        }
    }

    pub(crate) async fn acquire_permit(&self) -> Result<OwnedSemaphorePermit, ChannelError> {
        if self.is_shutdown() {
            return Err(ChannelError::ShuttingDown);
        }

        let permit = tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Err(ChannelError::ShuttingDown),
            permit = Arc::clone(&self.semaphore).acquire_owned() => {
                permit.map_err(|_closed| ChannelError::ShuttingDown)?
            }
        };

        if self.is_shutdown() {
            return Err(ChannelError::ShuttingDown);
        }

        Ok(permit)
    }

    pub(crate) async fn spawn_run<F>(&self, run_id: RunId, run: F) -> Result<(), ChannelError>
    where
        F: Future<Output = Result<(), KernelError>> + Send + 'static,
    {
        self.drain_finished().await;
        let mut tasks = self.tasks.lock().await;
        if self.is_shutdown() {
            return Err(ChannelError::ShuttingDown);
        }
        tasks.spawn(async move {
            let result = run.await;
            RunTaskOutcome { run_id, result }
        });
        Ok(())
    }

    /// Cooperative drain with a caller-supplied grace window.
    ///
    /// `Duration::ZERO` is the canonical sentinel for "use the
    /// builder-configured `shutdown_grace`" and falls back to
    /// [`Self::shutdown_grace`]. Any positive duration is honored
    /// directly as the drain timeout before `abort_all` kicks in.
    pub(crate) async fn shutdown_with_grace(&self, grace: Duration) {
        let effective = if grace.is_zero() {
            self.shutdown_grace
        } else {
            grace
        };
        self.cancel.cancel();
        let mut tasks = self.tasks.lock().await;
        if tasks.is_empty() {
            return;
        }

        let joined = timeout(effective, async {
            while let Some(result) = tasks.join_next().await {
                log_join_result(result);
            }
        })
        .await;

        if joined.is_err() {
            warn!(
                ?effective,
                "channel inbox shutdown grace elapsed; aborting in-flight runs"
            );
            tasks.abort_all();
            while let Some(result) = tasks.join_next().await {
                log_join_result(result);
            }
        }
    }

    pub(crate) async fn in_flight(&self) -> usize {
        self.drain_finished().await;
        self.tasks.lock().await.len()
    }

    async fn drain_finished(&self) {
        let mut tasks = self.tasks.lock().await;
        while let Some(result) = tasks.try_join_next() {
            log_join_result(result);
        }
    }
}

fn log_join_result(result: Result<RunTaskOutcome, JoinError>) {
    match result {
        Ok(outcome) => log_run_task_outcome(outcome),
        Err(err) => {
            error!("channel inbox kernel task join failed: {err}");
        }
    }
}

fn log_run_task_outcome(outcome: RunTaskOutcome) {
    let RunTaskOutcome { run_id, result } = outcome;
    if let Err(err) = &result {
        log_run_error(&run_id, err);
    }
}

fn log_run_error(run_id: &RunId, err: &KernelError) {
    match err {
        KernelError::Cancelled => debug!(%run_id, "channel inbox kernel run cancelled"),
        other => log_run_failure(run_id, other),
    }
}

fn log_run_failure(run_id: &RunId, err: &KernelError) {
    error!(%run_id, "channel inbox kernel run failed: {err}");
}

#[cfg(test)]
mod tests;
