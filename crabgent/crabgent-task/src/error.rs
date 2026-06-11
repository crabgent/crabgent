//! [`TaskError`]: errors surfaced by [`crate::TaskExecutor`] and
//! [`crate::TaskNotifier`].

use crabgent_core::error::KernelError;
use crabgent_store::error::StoreError;
use thiserror::Error;

/// Error type for task spawn / drain / notify operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TaskError {
    /// Persistence backend failed.
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// Kernel run failed mid-stream.
    #[error("kernel error: {0}")]
    Kernel(#[from] KernelError),

    /// Run exceeded the configured timeout.
    #[error("task timed out")]
    Timeout,

    /// Notifier reported a delivery failure.
    #[error("notify failed: {0}")]
    Notify(String),

    /// Catch-all for executor-level invariant violations.
    #[error("executor error: {0}")]
    Executor(String),
}

impl TaskError {
    /// Convenience: wrap any displayable value as [`Self::Notify`].
    pub fn notify(value: impl std::fmt::Display) -> Self {
        Self::Notify(value.to_string())
    }

    /// Convenience: wrap any displayable value as [`Self::Executor`].
    pub fn executor(value: impl std::fmt::Display) -> Self {
        Self::Executor(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_variant_displays_inner() {
        let err = TaskError::Store(StoreError::NotFound);
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn timeout_variant_has_static_message() {
        assert_eq!(TaskError::Timeout.to_string(), "task timed out");
    }

    #[test]
    fn notify_constructor_wraps_display() {
        let err = TaskError::notify("slack 503");
        assert!(matches!(err, TaskError::Notify(ref s) if s == "slack 503"));
    }

    #[test]
    fn executor_constructor_wraps_display() {
        let err = TaskError::executor("missing kernel handle");
        assert!(matches!(err, TaskError::Executor(ref s) if s == "missing kernel handle"));
    }

    #[test]
    fn store_error_converts_via_from() {
        let s: StoreError = StoreError::Conflict("dup".into());
        let t: TaskError = s.into();
        assert!(matches!(t, TaskError::Store(StoreError::Conflict(_))));
    }
}
