//! [`CronError`]: error type returned by [`crate::CronScheduler`] and the
//! [`crate::CronExecutor`] / [`crate::CronDelivery`] traits.

use crabgent_core::error::KernelError;
use crabgent_core::subject::InvalidSubjectError;
use crabgent_store::error::StoreError;
use thiserror::Error;

/// Error returned by cron-related operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CronError {
    /// Store-level failure (claim, finish, persist).
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// Kernel-level failure inside [`crate::CronExecutor`].
    #[error("kernel error: {0}")]
    Kernel(#[from] KernelError),

    /// The derived subject identity is malformed.
    #[error("invalid subject: {0}")]
    InvalidSubject(#[from] InvalidSubjectError),

    /// Job ran longer than the configured per-job timeout.
    #[error("cron job timed out")]
    Timeout,

    /// Schedule advancement failed (invalid cron expression after parse,
    /// timezone resolution failed, etc.).
    #[error("schedule error: {0}")]
    Schedule(String),

    /// Delivery channel rejected the message.
    #[error("delivery failed: {0}")]
    Delivery(String),

    /// Pre-processor reported an unrecoverable failure.
    #[error("pre-processor failed: {0}")]
    PreProcessor(String),

    /// Catch-all for executor/scheduler invariant violations.
    #[error("scheduler error: {0}")]
    Scheduler(String),
}

impl CronError {
    /// Build a [`CronError::Schedule`] from any displayable cause.
    pub fn schedule(value: impl std::fmt::Display) -> Self {
        Self::Schedule(value.to_string())
    }

    /// Build a [`CronError::Delivery`] from any displayable cause.
    pub fn delivery(value: impl std::fmt::Display) -> Self {
        Self::Delivery(value.to_string())
    }

    /// Build a [`CronError::PreProcessor`] from any displayable cause.
    pub fn pre_processor(value: impl std::fmt::Display) -> Self {
        Self::PreProcessor(value.to_string())
    }

    /// Build a [`CronError::Scheduler`] from any displayable cause.
    pub fn scheduler(value: impl std::fmt::Display) -> Self {
        Self::Scheduler(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_message_is_stable() {
        assert_eq!(CronError::Timeout.to_string(), "cron job timed out");
    }

    #[test]
    fn schedule_helper_wraps_display() {
        let e = CronError::schedule("bad expr");
        assert_eq!(e.to_string(), "schedule error: bad expr");
    }

    #[test]
    fn delivery_helper_wraps_display() {
        let e = CronError::delivery("offline");
        assert_eq!(e.to_string(), "delivery failed: offline");
    }

    #[test]
    fn pre_processor_helper_wraps_display() {
        let e = CronError::pre_processor("script crash");
        assert_eq!(e.to_string(), "pre-processor failed: script crash");
    }

    #[test]
    fn scheduler_helper_wraps_display() {
        let e = CronError::scheduler("invariant broken");
        assert_eq!(e.to_string(), "scheduler error: invariant broken");
    }

    #[test]
    fn store_error_converts() {
        let s = StoreError::NotFound;
        let e: CronError = s.into();
        assert!(matches!(e, CronError::Store(_)));
    }

    #[test]
    fn kernel_error_converts() {
        let k = KernelError::HookDenied {
            reason: "policy".into(),
        };
        let e: CronError = k.into();
        assert!(matches!(e, CronError::Kernel(_)));
    }

    #[test]
    fn invalid_subject_converts() {
        let e: CronError = InvalidSubjectError.into();
        assert!(matches!(e, CronError::InvalidSubject(_)));
    }
}
