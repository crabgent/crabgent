//! # crabgent-task
//!
//! Background-task execution on top of [`crabgent_store::TaskStore`] and
//! [`crabgent_core::Kernel`].
//!
//! [`TaskExecutor`] spawns a kernel run as a detached `tokio::task`,
//! drains the streaming events via [`crabgent_core::Kernel::run_streaming`],
//! persists incremental output with debounced writes through the
//! configured [`crabgent_store::TaskStore`], and dispatches a list of
//! [`TaskNotifier`]s on completion.
//!
//! Tasks are recoverable: incomplete `Running` records can be reset by
//! the store's [`crabgent_store::TaskStore::recover_stuck`] sweeper.

#![forbid(unsafe_code)]

pub mod drain;
pub mod error;
pub mod executor;
pub mod hook;
pub mod notifier;
pub mod observer;
pub mod request;

/// Subject attribute carrying a task's own [`crabgent_store::TaskId`] on the
/// kernel run the executor spawned for it. Named from the perspective of
/// child tasks created within that run: the tool layer reads it as the
/// parent link for depth control, while [`hook::TaskTranscriptHook`] reads
/// it to key transcript persistence.
pub const TASK_ID_ATTR: &str = "parent_task_id";

pub use drain::DrainOutcome;
pub use error::TaskError;
pub use executor::{DEFAULT_MAX_DEPTH, DEFAULT_MAX_PARALLEL, TaskExecutor};
pub use hook::TaskTranscriptHook;
pub use notifier::{NoopNotifier, TaskNotifier};
pub use observer::{
    NoopTaskObserver, TaskActivityEvent, TaskActivityKind, TaskActivityMeta, TaskObserver,
};
pub use request::TaskRequest;
