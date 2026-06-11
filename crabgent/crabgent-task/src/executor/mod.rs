//! [`TaskExecutor`]: spawns kernel runs as tracked background tasks,
//! persists incremental output, and dispatches completion notifiers.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crabgent_core::{CancelReason, Kernel};
use crabgent_log::{debug, error, warn};
use crabgent_store::TaskId;
use crabgent_store::traits::TaskStore;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::{JoinError, JoinSet};
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::error::TaskError;
use crate::request::TaskRequest;

mod blocking;
#[cfg(test)]
mod blocking_tests;
mod builders;
mod cancel;
#[cfg(test)]
mod classify_tests;
#[cfg(test)]
mod drain_tests;
mod drain_timeout;
mod finalize;
#[cfg(test)]
mod model_override_tests;
mod resume;
mod spawn;
#[cfg(test)]
mod tests;

pub const DEFAULT_TIMEOUT_SECS: u64 = 300;
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Default window [`TaskExecutor::shutdown`] waits for running tasks to
/// reach a safe pause boundary before force-cancelling them. Sized to
/// cover one provider turn plus tool flush.
const DEFAULT_PAUSE_GRACE: Duration = Duration::from_secs(30);
/// Default staleness threshold for boot-time orphan adoption. `0` adopts
/// every `Running` row, which is correct for single-process deployments
/// (any `Running` row at startup belongs to a dead process).
const DEFAULT_ORPHAN_STALE_SECS: i64 = 0;
/// Default cap on resume attempts per task. Breaks pause/resume crash
/// loops on poison tasks: the claim CAS rejects further resumes and the
/// scan finishes the task `Failed("resume limit exceeded")`.
const DEFAULT_MAX_RESUMES: u32 = 3;
const DEFAULT_PROGRESS_DEBOUNCE_MS: u64 = 500;
const DEFAULT_OUTPUT_DEBOUNCE_BYTES: usize = 256;
pub const DEFAULT_MAX_DEPTH: usize = 3;
pub const DEFAULT_MAX_PARALLEL: usize = 5;
const TIMEOUT_MESSAGE: &str = "task timed out";
const CANCELLED_MESSAGE: &str = "task cancelled";
pub(crate) const RESUME_LIMIT_MESSAGE: &str = "resume limit exceeded";

/// Per-task control handles: the cancel child token and the shared
/// cancel-reason attribution cell installed on the task's `RunRequest`.
pub(crate) struct TaskHandles {
    pub(crate) cancel: CancellationToken,
    pub(crate) reason: Arc<OnceLock<CancelReason>>,
}

/// Spawns kernel runs as tracked `tokio::task`s and persists their
/// output through a [`TaskStore`].
///
/// Construct once with [`Self::new`] (and optional builder methods),
/// then call [`Self::spawn`] repeatedly. Each `spawn` returns the
/// freshly created [`TaskId`] immediately; the actual run continues
/// in the executor's tracked task set.
pub struct TaskExecutor<S: TaskStore> {
    pub(crate) store: Arc<S>,
    pub(crate) notifiers: Vec<Arc<dyn crate::notifier::TaskNotifier>>,
    pub(crate) observers: Vec<Arc<dyn crate::observer::TaskObserver>>,
    pub(crate) timeout: Duration,
    pub(crate) shutdown_grace: Duration,
    pub(crate) progress_debounce: Duration,
    pub(crate) output_debounce_bytes: usize,
    pub(crate) max_depth: usize,
    pub(crate) max_parallel: usize,
    pub(crate) semaphore: Arc<Semaphore>,
    pub(crate) cancel: CancellationToken,
    /// Executor-wide cooperative pause signal. Each task gets a child via
    /// `RunRequest.pause`; firing the root makes every run exit
    /// `Outcome::Paused` at its next safe boundary without interrupting
    /// in-flight provider or tool futures.
    pub(crate) pause_root: CancellationToken,
    /// Window [`Self::shutdown`] waits for cooperative pause before
    /// force-cancelling stragglers.
    pub(crate) pause_grace: Duration,
    /// Staleness threshold for [`Self::resume_paused`] orphan adoption.
    pub(crate) orphan_stale_secs: i64,
    /// Resume-attempt cap enforced by the claim CAS.
    pub(crate) max_resumes: u32,
    /// Per-task control handles. Global shutdown via `self.cancel.cancel()`
    /// propagates to all cancel children. Per-task `cancel(&id)` removes
    /// the entry, stamps user cancel intent, and fires the token.
    pub(crate) cancel_handles: Arc<Mutex<HashMap<TaskId, TaskHandles>>>,
    pub(crate) tasks: Mutex<JoinSet<TaskId>>,
}

impl<S: TaskStore> TaskExecutor<S> {
    #[must_use]
    pub fn new(store: Arc<S>) -> Self {
        Self {
            store,
            notifiers: Vec::new(),
            observers: Vec::new(),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
            progress_debounce: Duration::from_millis(DEFAULT_PROGRESS_DEBOUNCE_MS),
            output_debounce_bytes: DEFAULT_OUTPUT_DEBOUNCE_BYTES,
            max_depth: DEFAULT_MAX_DEPTH,
            max_parallel: DEFAULT_MAX_PARALLEL,
            semaphore: Arc::new(Semaphore::new(DEFAULT_MAX_PARALLEL)),
            cancel: CancellationToken::new(),
            pause_root: CancellationToken::new(),
            pause_grace: DEFAULT_PAUSE_GRACE,
            orphan_stale_secs: DEFAULT_ORPHAN_STALE_SECS,
            max_resumes: DEFAULT_MAX_RESUMES,
            cancel_handles: Arc::new(Mutex::new(HashMap::new())),
            tasks: Mutex::new(JoinSet::new()),
        }
    }

    /// Pause-first shutdown. Sequence:
    /// 1. Fire the pause root: every running task exits `Outcome::Paused`
    ///    at its next safe boundary and is stored `Paused(Shutdown)`; new
    ///    spawns are rejected.
    /// 2. Wait up to [`Self::pause_grace`] for the cooperative drain.
    /// 3. Stragglers (hung tool calls): stamp `CancelReason::Shutdown` on
    ///    their attribution cells (first-write-wins keeps an earlier user
    ///    cancel), fire the cancel root, and wait up to
    ///    [`Self::shutdown_grace`]; the drive path stores them
    ///    `Paused(Forced)`.
    /// 4. Abort whatever still runs. Rows that never got their store
    ///    write stay `Running` and are adopted as crash orphans by
    ///    [`Self::resume_paused`] at the next startup.
    ///
    /// Total wait is bounded by `pause_grace + shutdown_grace`; repeated
    /// calls are idempotent.
    pub async fn shutdown(&self) {
        self.pause_root.cancel();
        // Take the JoinSet out under a short-lived lock instead of holding the
        // lock across the whole await. A concurrent spawn() would otherwise
        // block for the entire grace window. New spawns after this point are
        // rejected by the fired pause root; a spawn that already held its
        // permit lands in the replacement set and converges via boot-time
        // orphan adoption.
        let mut tasks = {
            let mut guard = self.tasks.lock().await;
            std::mem::take(&mut *guard)
        };
        if tasks.is_empty() {
            self.cancel.cancel();
            return;
        }

        let paused = time::timeout(self.pause_grace, drain_all(&mut tasks)).await;
        if paused.is_err() {
            self.force_cancel_stragglers(&mut tasks).await;
        }
        self.cancel.cancel();
    }

    /// Phase 3+4 of [`Self::shutdown`]: attribute, cancel, drain within
    /// `shutdown_grace`, abort leftovers.
    async fn force_cancel_stragglers(&self, tasks: &mut JoinSet<TaskId>) {
        log_pause_grace_elapsed(self.pause_grace);
        self.stamp_shutdown_reasons().await;
        self.cancel.cancel();
        let joined = time::timeout(self.shutdown_grace, drain_all(tasks)).await;
        if joined.is_err() {
            log_shutdown_grace_elapsed(self.shutdown_grace);
            tasks.abort_all();
            drain_all(tasks).await;
        }
    }

    /// Attribute the upcoming force-cancel to shutdown on every live task.
    /// First-write-wins: a task the user already cancelled keeps its
    /// `StopPattern` intent and therefore fails instead of pausing.
    async fn stamp_shutdown_reasons(&self) {
        let handles = self.cancel_handles.lock().await;
        for task_handles in handles.values() {
            let _rejected = task_handles.reason.set(CancelReason::Shutdown);
        }
    }

    pub async fn in_flight(&self) -> usize {
        self.drain_finished().await;
        self.tasks.lock().await.len()
    }

    #[must_use]
    pub const fn max_depth(&self) -> usize {
        self.max_depth
    }

    #[must_use]
    pub const fn max_parallel(&self) -> usize {
        self.max_parallel
    }

    #[must_use]
    pub const fn shutdown_grace(&self) -> Duration {
        self.shutdown_grace
    }

    async fn drain_finished(&self) {
        let mut tasks = self.tasks.lock().await;
        drain_finished_locked(&mut tasks);
    }
}

impl<S> TaskExecutor<S>
where
    S: TaskStore + 'static,
{
    /// Insert a `Running` task record, spawn a detached drain loop, and
    /// return the new [`TaskId`]. The kernel run is driven by the
    /// spawned task; callers do not need to await it.
    pub async fn spawn(&self, kernel: Arc<Kernel>, req: TaskRequest) -> Result<TaskId, TaskError> {
        spawn::do_spawn(self, kernel, req).await
    }

    /// Cancel a running task by id.
    ///
    /// Returns `true` when a live task token existed and was cancelled.
    pub async fn cancel(&self, id: &TaskId) -> bool {
        cancel::do_cancel(self, id).await
    }

    /// Spawn a task and wait for its persisted completion record.
    /// During a shutdown the returned record can be non-terminal
    /// `Paused`: the waiter is released with the paused snapshot and the
    /// task resumes after restart with no waiter attached.
    pub async fn spawn_blocking(
        &self,
        kernel: Arc<Kernel>,
        req: TaskRequest,
        timeout: Option<Duration>,
    ) -> Result<crabgent_store::records::Task, TaskError> {
        blocking::do_spawn_blocking(self, kernel, req, timeout).await
    }

    /// Startup primitive: adopt crash orphans, then claim and respawn
    /// every paused task from its persisted transcript and resume spec.
    /// Children resume before their parents so a re-attached blocking
    /// parent finds its child already running. Idempotent: the claim CAS
    /// yields exactly one winner per task, and a crash anywhere in this
    /// scan converges through orphan adoption on the next call. Run this
    /// BEFORE scheduling any `recover_stuck` sweeper.
    ///
    /// Returns the ids of the tasks that were respawned.
    ///
    /// # Errors
    ///
    /// Returns the first store error encountered while scanning; already
    /// respawned tasks keep running.
    pub async fn resume_paused(&self, kernel: Arc<Kernel>) -> Result<Vec<TaskId>, TaskError> {
        resume::do_resume_paused(self, move |_task| Some(Arc::clone(&kernel))).await
    }

    /// Multi-agent variant of [`Self::resume_paused`] for hosts that
    /// share one task store across several kernels/executors. The
    /// resolver runs BEFORE any claim and matches each paused task to
    /// the kernel that should resume it (typically by inspecting
    /// `task.resume_spec.subject_attrs`, e.g. an `agent` attribute);
    /// returning `None` leaves the task untouched for a sibling
    /// executor's scan, so no executor steals another agent's tasks.
    /// Children-before-parents ordering applies within the selected set.
    /// Orphan adoption stays agent-neutral: it only folds stale `Running`
    /// rows into `Paused(Crash)` without binding them to this executor,
    /// and repeated adoption passes are idempotent.
    ///
    /// # Errors
    ///
    /// Returns the first store error encountered while scanning; already
    /// respawned tasks keep running.
    pub async fn resume_paused_with<F>(&self, resolve: F) -> Result<Vec<TaskId>, TaskError>
    where
        F: Fn(&crabgent_store::records::Task) -> Option<Arc<Kernel>> + Send,
    {
        resume::do_resume_paused(self, resolve).await
    }
}

fn log_pause_grace_elapsed(pause_grace: Duration) {
    warn!(
        ?pause_grace,
        "task executor pause grace elapsed; force-cancelling stragglers"
    );
}

fn log_shutdown_grace_elapsed(shutdown_grace: Duration) {
    warn!(
        ?shutdown_grace,
        "task executor shutdown grace elapsed; aborting running tasks"
    );
}

/// Join every task in the set, logging each result.
async fn drain_all(tasks: &mut JoinSet<TaskId>) {
    while let Some(result) = tasks.join_next().await {
        log_task_join(result);
    }
}

pub(crate) fn drain_finished_locked(tasks: &mut JoinSet<TaskId>) {
    while let Some(result) = tasks.try_join_next() {
        log_task_join(result);
    }
}

pub(crate) fn log_task_join(result: Result<TaskId, JoinError>) {
    match result {
        Ok(task_id) => log_task_join_success(&task_id),
        Err(err) => log_task_join_error(&err),
    }
}

fn log_task_join_success(task_id: &TaskId) {
    debug!(task_id = %task_id, "task executor: task joined");
}

fn log_task_join_error(err: &JoinError) {
    error!("task executor: spawned task join failed: {err}");
}
