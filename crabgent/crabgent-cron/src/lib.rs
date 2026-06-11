//! `crabgent-cron`: claim-based cron scheduler for crabgent kernel runs.
//!
//! Wires a [`CronStore`] (Memory, `SQLite`, custom) to a kernel via the
//! [`CronExecutor`] trait. The default executor [`KernelCronExecutor`]
//! calls `kernel.run()` with the (possibly augmented) prompt. Pre-processors
//! (skip/augment/deliver) run before the kernel; deliveries (Slack, Matrix,
//! ...) fire after the kernel produces its final text.
//!
//! [`CronScheduler`] ticks every `tick_interval`, atomically claims due
//! jobs from the store, dispatches each onto a [`tokio::task::JoinSet`]
//! capped by a semaphore, and advances `next_run` via [`schedule::next_run`]
//! after the run finishes.
//!
//! [`CronStore`]: crabgent_store::traits::CronStore

/// Keep this alias in sync with `tracing_test::traced_test` and `#[instrument]`
/// macro expectations, so tests keep using the canonical `tracing` path while
/// crate internals stay coupled to `crabgent_log`.
#[cfg(test)]
extern crate crabgent_log as tracing;

mod delivery;
mod error;
mod executor;
mod observer;
mod pre_processor;
mod schedule;
mod scheduler;

pub use delivery::{CronDelivery, NoopDelivery};
pub use error::CronError;
pub use executor::{CronExecCtx, CronExecResult, CronExecutor, KernelCronExecutor};
pub use observer::{
    CronActivityEvent, CronActivityKind, CronJobActivityMeta, CronObserver, NoopCronObserver,
};
pub use pre_processor::{CronPreProcessResult, CronPreProcessor};
pub use schedule::{next_run, validate_cron_expr};
pub use scheduler::CronScheduler;
