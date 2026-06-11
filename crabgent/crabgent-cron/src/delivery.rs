//! [`CronDelivery`]: trait for shipping cron-job results to a channel.
//!
//! Implementors push the cron-job's final assistant text to a channel
//! (Slack, Matrix, Telegram, ...) and report whether delivery succeeded.
//! The scheduler calls every registered delivery sequentially after the
//! [`CronExecutor`] finishes successfully. Returning `Ok(false)` means
//! delivery was attempted but the channel rejected the message; `Err(_)`
//! means the attempt itself failed.
//!
//! [`CronExecutor`]: crate::CronExecutor

use async_trait::async_trait;
use crabgent_store::records::CronJob;

use crate::error::CronError;

/// Delivers cron-job result text to a channel.
#[async_trait]
pub trait CronDelivery: Send + Sync {
    /// Send `message` for `job`. Returns `Ok(true)` on success.
    async fn deliver(&self, job: &CronJob, message: &str) -> Result<bool, CronError>;
}

/// Delivery that drops every message on the floor. Useful as a default
/// when callers do not need delivery, or as a sentinel in tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopDelivery;

#[async_trait]
impl CronDelivery for NoopDelivery {
    async fn deliver(&self, _job: &CronJob, _message: &str) -> Result<bool, CronError> {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crabgent_core::MemoryScope;
    use crabgent_store::CronJobId;
    use crabgent_store::records::CronSchedule;
    use serde_json::json;

    fn fixture() -> CronJob {
        CronJob {
            id: CronJobId::new(),
            name: "demo".into(),
            scope: MemoryScope::default(),
            prompt: "p".into(),
            schedule: CronSchedule::every(60),
            enabled: true,
            run_once: false,
            model_override: None,
            reasoning_effort_override: None,
            pre_command: None,
            delivery_ctx: json!({}),
            last_run: None,
            next_run: Utc::now(),
            created_at: Utc::now(),
            claimed_at: None,
        }
    }

    #[tokio::test]
    async fn noop_returns_ok_true() {
        let d = NoopDelivery;
        let job = fixture();
        assert!(d.deliver(&job, "hello").await.expect("test result"));
    }

    #[tokio::test]
    async fn noop_clone_is_independent() {
        let a = NoopDelivery;
        let b = a;
        let job = fixture();
        assert!(a.deliver(&job, "x").await.expect("test result"));
        assert!(b.deliver(&job, "y").await.expect("test result"));
    }
}
