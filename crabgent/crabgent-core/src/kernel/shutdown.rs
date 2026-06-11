//! `Kernel::shutdown` graceful drain of in-flight runs.

use std::mem;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio::time;

use crate::hook_chain::HookChain;
use crate::kernel::Kernel;
use crate::run::reap_finished_drivers;

/// Poll interval while `shutdown_with_pause` waits for in-flight runs to
/// reach a safe pause boundary.
const PAUSE_POLL_INTERVAL: Duration = Duration::from_millis(25);

impl Kernel {
    /// Stop accepting new runs and drain in-flight runs cooperatively.
    ///
    /// Sequence:
    /// 1. Cancel [`Kernel::shutdown_token`]. New calls to
    ///    [`Kernel::run`] / [`Kernel::run_streaming`] return
    ///    `KernelError::ShuttingDown` immediately; active runs observe
    ///    the propagated cancel through their per-run token (directly
    ///    when the caller passed `None`, via the shutdown watcher task
    ///    when the caller supplied its own cancel token).
    /// 2. Run [`crate::Hook::on_kernel_shutdown`] callbacks with the
    ///    shutdown token already cancelled.
    /// 3. Take ownership of the running `JoinSet` and drain it within
    ///    [`Kernel::shutdown_grace`].
    /// 4. If the grace window elapses with tasks still running, abort
    ///    them and drain again so no detached task survives.
    ///
    /// Repeated calls are idempotent: the token stays cancelled, and the
    /// second call drains whatever new tracked tasks arrived during the
    /// first (none, in well-behaved callers).
    pub async fn shutdown(&self) {
        self.shutdown_token.cancel();
        self.hooks
            .apply_on_kernel_shutdown(&self.shutdown_token)
            .await;
        let mut jset = take_running(&self.running);
        if jset.is_empty() {
            return;
        }
        let graceful = time::timeout(self.shutdown_grace, drain_set(&mut jset, &self.hooks)).await;
        if graceful.is_err() {
            jset.abort_all();
            drain_set(&mut jset, &self.hooks).await;
        }
    }

    /// Request a cooperative pause of all in-flight runs. Every run exits
    /// with `Outcome::Paused` at its next safe boundary (turn start or
    /// between tool dispatches); in-flight provider and tool futures are
    /// never interrupted. Idempotent. New runs are still accepted and
    /// observe the signal at their first turn boundary; reject new work
    /// at the adapter layer when pausing ahead of a shutdown.
    pub fn request_pause(&self) {
        self.pause_token.cancel();
    }

    /// Pause-first shutdown: fire the kernel-wide pause signal, wait up
    /// to `pause_grace` for in-flight runs to reach a safe boundary and
    /// exit `Outcome::Paused`, then run the regular [`Kernel::shutdown`]
    /// sequence (cancel, hooks, drain within `shutdown_grace`, abort).
    /// Total wait is bounded by `pause_grace + shutdown_grace`; nothing
    /// can delay shutdown indefinitely. Repeated calls are idempotent
    /// like [`Kernel::shutdown`].
    pub async fn shutdown_with_pause(&self, pause_grace: Duration) {
        self.pause_token.cancel();
        let deadline = time::Instant::now() + pause_grace;
        while time::Instant::now() < deadline {
            if self.running_idle() {
                break;
            }
            time::sleep(PAUSE_POLL_INTERVAL).await;
        }
        self.shutdown().await;
    }

    /// Reap finished drivers and report whether any run is still active.
    fn running_idle(&self) -> bool {
        let mut guard = self
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reap_finished_drivers(&mut guard, &self.hooks);
        guard.is_empty()
    }
}

fn take_running(running: &std::sync::Mutex<JoinSet<()>>) -> JoinSet<()> {
    let mut guard = running
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    mem::take(&mut *guard)
}

async fn drain_set(jset: &mut JoinSet<()>, hooks: &HookChain) {
    while let Some(joined) = jset.join_next().await {
        if let Err(err) = joined {
            // Surface the JoinError (panic or abort) through the hook
            // chain so observers like `crabgent-hook-log` can log it.
            // Aborts after the grace window flow through here too; that
            // is intentional, the bridge tags them as such.
            hooks.apply_on_kernel_shutdown_task_panic(&err).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio::time::{Instant, sleep};

    use crate::error::ProviderError;
    use crate::hook::{Hook, RunCtx};
    use crate::kernel::Kernel;
    use crate::model::ModelInfo;
    use crate::policy::AllowAllPolicy;
    use crate::provider::{Provider, ProviderCapabilities};
    use crate::types::{LlmRequest, LlmResponse};
    use async_trait::async_trait;
    use tokio::task::JoinError;
    use tokio_util::sync::CancellationToken;

    struct InertProvider;

    #[async_trait]
    impl Provider for InertProvider {
        async fn complete(
            &self,
            _req: &LlmRequest,
            _ctx: &RunCtx,
            _cancel: Option<&CancellationToken>,
        ) -> Result<LlmResponse, ProviderError> {
            Err(ProviderError::Other("inert".into()))
        }
        fn name(&self) -> &'static str {
            "inert"
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }
        fn models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo::minimal("inert", "inert")]
        }
    }

    fn test_kernel(grace: Duration) -> Arc<Kernel> {
        Arc::new(
            Kernel::builder()
                .provider(InertProvider)
                .policy(AllowAllPolicy)
                .with_graceful_shutdown(grace)
                .build(),
        )
    }

    #[tokio::test]
    async fn shutdown_with_empty_joinset_is_fast() {
        let kernel = test_kernel(Duration::from_mins(1));
        let start = Instant::now();
        kernel.shutdown().await;
        assert!(start.elapsed() < Duration::from_millis(100));
        assert!(kernel.shutdown_token().is_cancelled());
    }

    #[tokio::test]
    async fn shutdown_drains_cooperative_tasks_within_grace() {
        let kernel = test_kernel(Duration::from_secs(5));
        let inner_cancel = kernel.shutdown_token().child_token();
        let inner_cancel_clone = inner_cancel.clone();
        {
            let mut guard = kernel.running.lock().expect("poisoned");
            guard.spawn(async move {
                tokio::select! {
                    () = inner_cancel_clone.cancelled() => {}
                    () = sleep(Duration::from_mins(1)) => {}
                }
            });
        }
        let start = Instant::now();
        kernel.shutdown().await;
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(inner_cancel.is_cancelled());
    }

    #[tokio::test]
    async fn shutdown_aborts_uncooperative_tasks_after_grace() {
        let kernel = test_kernel(Duration::from_millis(200));
        {
            let mut guard = kernel.running.lock().expect("poisoned");
            // task ignores cancel: only ends on abort.
            guard.spawn(async {
                loop {
                    sleep(Duration::from_mins(1)).await;
                }
            });
        }
        let start = Instant::now();
        kernel.shutdown().await;
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(200));
        assert!(elapsed < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let kernel = test_kernel(Duration::from_millis(50));
        kernel.shutdown().await;
        kernel.shutdown().await;
        assert!(kernel.shutdown_token().is_cancelled());
    }

    struct CounterHook {
        counter: Arc<AtomicUsize>,
        pre_drain_flag: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Hook for CounterHook {
        async fn on_kernel_shutdown(&self, _token: &CancellationToken) {
            self.counter.fetch_add(1, Ordering::Relaxed);
            self.pre_drain_flag.store(true, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn shutdown_invokes_on_kernel_shutdown_hook_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let pre_drain_flag = Arc::new(AtomicBool::new(false));
        let kernel = Arc::new(
            Kernel::builder()
                .provider(InertProvider)
                .policy(AllowAllPolicy)
                .add_hook(CounterHook {
                    counter: counter.clone(),
                    pre_drain_flag: pre_drain_flag.clone(),
                })
                .with_graceful_shutdown(Duration::from_secs(1))
                .build(),
        );
        kernel.shutdown().await;
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    // Single-thread flavor pinned explicitly: the hook-before-drain
    // ordering relies on cooperative scheduling; multi-thread would race
    // the spawned cooperative task against the hook future.
    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_fires_hook_before_drain() {
        let counter = Arc::new(AtomicUsize::new(0));
        let pre_drain_flag = Arc::new(AtomicBool::new(false));
        let kernel = Arc::new(
            Kernel::builder()
                .provider(InertProvider)
                .policy(AllowAllPolicy)
                .add_hook(CounterHook {
                    counter: counter.clone(),
                    pre_drain_flag: pre_drain_flag.clone(),
                })
                .with_graceful_shutdown(Duration::from_secs(5))
                .build(),
        );
        let observed = Arc::new(AtomicBool::new(false));
        let observed_clone = observed.clone();
        let flag_clone = pre_drain_flag.clone();
        let inner_cancel = kernel.shutdown_token().child_token();
        let inner_cancel_clone = inner_cancel.clone();
        {
            let mut guard = kernel.running.lock().expect("poisoned");
            guard.spawn(async move {
                tokio::select! {
                    () = inner_cancel_clone.cancelled() => {
                        if flag_clone.load(Ordering::Relaxed) {
                            observed_clone.store(true, Ordering::Relaxed);
                        }
                    }
                    () = sleep(Duration::from_secs(5)) => {}
                }
            });
        }
        kernel.shutdown().await;
        assert!(
            observed.load(Ordering::Relaxed),
            "hook must run before drain"
        );
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    struct PanicCounterHook {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Hook for PanicCounterHook {
        async fn on_kernel_shutdown_task_panic(&self, _err: &JoinError) {
            self.counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn shutdown_dispatches_panic_hook_on_task_panic() {
        let counter = Arc::new(AtomicUsize::new(0));
        let kernel = Arc::new(
            Kernel::builder()
                .provider(InertProvider)
                .policy(AllowAllPolicy)
                .add_hook(PanicCounterHook {
                    counter: counter.clone(),
                })
                .with_graceful_shutdown(Duration::from_secs(1))
                .build(),
        );
        {
            let mut guard = kernel.running.lock().expect("poisoned");
            guard.spawn(async {
                panic!("intentional shutdown-drain panic");
            });
        }
        kernel.shutdown().await;
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn shutdown_dispatches_panic_hook_on_abort_after_grace() {
        let counter = Arc::new(AtomicUsize::new(0));
        let kernel = Arc::new(
            Kernel::builder()
                .provider(InertProvider)
                .policy(AllowAllPolicy)
                .add_hook(PanicCounterHook {
                    counter: counter.clone(),
                })
                .with_graceful_shutdown(Duration::from_millis(50))
                .build(),
        );
        {
            let mut guard = kernel.running.lock().expect("poisoned");
            guard.spawn(async {
                loop {
                    sleep(Duration::from_mins(1)).await;
                }
            });
        }
        kernel.shutdown().await;
        // Abort surfaces as JoinError::is_cancelled and is reported via
        // the same hook surface as a panic.
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn default_grace_is_five_seconds() {
        let kernel = Kernel::builder()
            .provider(InertProvider)
            .policy(AllowAllPolicy)
            .build();
        assert_eq!(kernel.shutdown_grace(), Duration::from_secs(5));
    }
}
