//! Typing-indicator surface for channel adapters.
//!
//! `TypingIndicator` is the channel-agnostic trait that adapter crates
//! implement to surface a "the agent is working" signal. `TypingHook` is
//! the bridge from kernel run lifecycle (`on_session_start` / `on_stop`)
//! into the indicator. The trait is intentionally minimal: start and stop
//! both take a `RunCtx` reference, so impls can read `subject.attrs` to
//! discover the live channel target and short-circuit when the run does
//! not belong to their channel.
//!
//! Hook ordering: one `start()` call per run (at `on_session_start`), one
//! `stop()` call per run (at `on_stop`, regardless of `Outcome`). The
//! adapter owns any heartbeat loop the protocol requires (Matrix typing
//! expires after 30s, Telegram after 5s).
//!
//! Failures from the indicator are fail-open: the hook logs and continues.
//! Typing is a UX signal, not a policy gate.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{Decision, Hook, Outcome, RunCtx};
use thiserror::Error;

/// Errors surfaced by `TypingIndicator` implementations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TypingError {
    /// Transport failure while sending the typing signal.
    #[error("typing indicator transport failed: {0}")]
    Transport(String),
}

/// Crate-local result alias for typing-indicator operations.
pub type TypingResult<T> = Result<T, TypingError>;

/// Channel-agnostic typing indicator.
///
/// Implementations decide whether the run targets their channel by
/// inspecting `ctx.subject.attrs`. A run that belongs to a different
/// channel should be a no-op, not an error.
#[async_trait]
pub trait TypingIndicator: Send + Sync {
    /// Start the typing indicator for the given run.
    ///
    /// Called once per `Kernel::run` invocation from `on_session_start`.
    /// Implementations spawn any background heartbeat loop here.
    async fn start(&self, ctx: &RunCtx) -> TypingResult<()>;

    /// Stop the typing indicator for the given run.
    ///
    /// Called once per `Kernel::run` invocation from `on_stop`, for every
    /// `Outcome` variant. Implementations must abort any background
    /// heartbeat loop here. Idempotent: a `stop()` call without a matching
    /// `start()` returns `Ok(())`.
    async fn stop(&self, ctx: &RunCtx) -> TypingResult<()>;
}

/// Typing indicator that accepts and discards every call.
///
/// Default for builders that wire `TypingHook` without channel support.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTypingIndicator;

#[async_trait]
impl TypingIndicator for NoopTypingIndicator {
    async fn start(&self, _ctx: &RunCtx) -> TypingResult<()> {
        Ok(())
    }

    async fn stop(&self, _ctx: &RunCtx) -> TypingResult<()> {
        Ok(())
    }
}

/// Hook bridge that drives a `TypingIndicator` from run lifecycle events.
///
/// Wire one `TypingHook` per channel adapter via `KernelBuilder::with_hook`.
/// Multiple typing hooks may coexist: each adapter-specific indicator
/// short-circuits on `ctx.subject.attrs["channel"]` mismatch.
pub struct TypingHook {
    indicator: Arc<dyn TypingIndicator>,
}

impl std::fmt::Debug for TypingHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypingHook").finish_non_exhaustive()
    }
}

impl TypingHook {
    /// Build a hook bridge for the given indicator.
    #[must_use]
    pub fn new(indicator: Arc<dyn TypingIndicator>) -> Self {
        Self { indicator }
    }
}

#[async_trait]
impl Hook for TypingHook {
    async fn on_session_start(&self, ctx: &RunCtx) -> Decision<()> {
        if let Err(err) = self.indicator.start(ctx).await {
            crabgent_log::warn!(
                run_id = %ctx.run_id,
                subject_id = %ctx.subject.id(),
                error = %err,
                "typing indicator start failed",
            );
        }
        Decision::Continue
    }

    async fn on_stop(&self, ctx: &RunCtx, _outcome: &Outcome) {
        if let Err(err) = self.indicator.stop(ctx).await {
            crabgent_log::warn!(
                run_id = %ctx.run_id,
                subject_id = %ctx.subject.id(),
                error = %err,
                "typing indicator stop failed",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::{RunId, Subject};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct CountingIndicator {
        starts: AtomicUsize,
        stops: AtomicUsize,
        fail_start: bool,
        fail_stop: bool,
    }

    #[async_trait]
    impl TypingIndicator for CountingIndicator {
        async fn start(&self, _ctx: &RunCtx) -> TypingResult<()> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            if self.fail_start {
                Err(TypingError::Transport("boom-start".into()))
            } else {
                Ok(())
            }
        }

        async fn stop(&self, _ctx: &RunCtx) -> TypingResult<()> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            if self.fail_stop {
                Err(TypingError::Transport("boom-stop".into()))
            } else {
                Ok(())
            }
        }
    }

    fn ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("typing-user"))
    }

    #[tokio::test]
    async fn noop_indicator_returns_ok() {
        let ind = NoopTypingIndicator;
        ind.start(&ctx()).await.expect("noop start");
        ind.stop(&ctx()).await.expect("noop stop");
    }

    #[tokio::test]
    async fn typing_hook_dispatches_start_and_stop_once() {
        let ind: Arc<CountingIndicator> = Arc::new(CountingIndicator::default());
        let hook = TypingHook::new(ind.clone());
        let c = ctx();

        let decision = hook.on_session_start(&c).await;
        assert!(matches!(decision, Decision::Continue));
        hook.on_stop(&c, &Outcome::Completed("done".into())).await;

        assert_eq!(ind.starts.load(Ordering::SeqCst), 1);
        assert_eq!(ind.stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn typing_hook_swallows_indicator_errors() {
        let ind: Arc<CountingIndicator> = Arc::new(CountingIndicator {
            fail_start: true,
            fail_stop: true,
            ..CountingIndicator::default()
        });
        let hook = TypingHook::new(ind.clone());
        let c = ctx();

        let decision = hook.on_session_start(&c).await;
        assert!(matches!(decision, Decision::Continue));
        hook.on_stop(&c, &Outcome::Errored("nope".into())).await;

        assert_eq!(ind.starts.load(Ordering::SeqCst), 1);
        assert_eq!(ind.stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn typing_hook_stops_on_every_outcome() {
        let outcomes = [
            Outcome::Completed("ok".into()),
            Outcome::MaxTurnsExceeded,
            Outcome::Cancelled,
            Outcome::Errored("err".into()),
        ];

        let ind: Arc<CountingIndicator> = Arc::new(CountingIndicator::default());
        let hook = TypingHook::new(ind.clone());
        let c = ctx();

        for outcome in &outcomes {
            hook.on_stop(&c, outcome).await;
        }

        assert_eq!(ind.stops.load(Ordering::SeqCst), outcomes.len());
    }

    #[tokio::test]
    async fn typing_indicator_is_object_safe() {
        let ind: Arc<dyn TypingIndicator> = Arc::new(NoopTypingIndicator);
        ind.start(&ctx()).await.expect("trait object start");
        ind.stop(&ctx()).await.expect("trait object stop");
    }
}
