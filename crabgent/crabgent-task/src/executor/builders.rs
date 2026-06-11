use std::sync::Arc;
use std::time::Duration;

use crabgent_store::traits::TaskStore;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::notifier::TaskNotifier;

use super::TaskExecutor;

impl<S: TaskStore> TaskExecutor<S> {
    #[must_use]
    pub fn with_notifier(mut self, n: Arc<dyn TaskNotifier>) -> Self {
        self.notifiers.push(n);
        self
    }

    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn crate::observer::TaskObserver>) -> Self {
        self.observers.push(observer);
        self
    }

    #[must_use]
    pub const fn with_timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    #[must_use]
    pub const fn with_shutdown_grace(mut self, d: Duration) -> Self {
        self.shutdown_grace = d;
        self
    }

    /// Window [`TaskExecutor::shutdown`] waits for running tasks to reach
    /// a safe pause boundary before force-cancelling them. Default 30 s.
    #[must_use]
    pub const fn with_pause_grace(mut self, d: Duration) -> Self {
        self.pause_grace = d;
        self
    }

    /// Staleness threshold for boot-time orphan adoption in
    /// [`TaskExecutor::resume_paused`]. Default 0 (adopt every `Running`
    /// row; correct for single-process deployments). Raise it when
    /// multiple executors share one store so live tasks of a sibling
    /// process (heartbeating `updated_at` via transcript writes) are not
    /// adopted. The heartbeat pauses for the duration of a silent tool
    /// call (transcript writes happen per completed message), so a
    /// multi-process threshold must exceed the longest expected tool
    /// call.
    #[must_use]
    pub const fn with_orphan_stale_secs(mut self, secs: i64) -> Self {
        self.orphan_stale_secs = secs;
        self
    }

    /// Cap on resume attempts per task (claim CAS guard). Default 3.
    #[must_use]
    pub const fn with_max_resumes(mut self, n: u32) -> Self {
        self.max_resumes = n;
        self
    }

    #[must_use]
    pub fn with_cancel(mut self, parent: &CancellationToken) -> Self {
        self.cancel = parent.child_token();
        self
    }

    #[must_use]
    pub const fn with_progress_debounce(mut self, d: Duration) -> Self {
        self.progress_debounce = d;
        self
    }

    #[must_use]
    pub const fn with_output_debounce_bytes(mut self, n: usize) -> Self {
        self.output_debounce_bytes = n;
        self
    }

    #[must_use]
    pub const fn with_max_depth(mut self, n: usize) -> Self {
        self.max_depth = n;
        self
    }

    #[must_use]
    pub fn with_max_parallel(mut self, n: usize) -> Self {
        self.max_parallel = n;
        self.semaphore = Arc::new(Semaphore::new(n));
        self
    }
}
