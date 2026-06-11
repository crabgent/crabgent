//! [`CronPreProcessor`]: trait for inspecting and possibly augmenting a
//! cron job before it reaches the kernel.
//!
//! The scheduler calls every registered pre-processor sequentially. The
//! first one returning a non-`Passthrough` outcome decides:
//!
//! - [`CronPreProcessResult::Skip`]: the run is suppressed entirely; the
//!   scheduler just advances `next_run` and moves on.
//! - [`CronPreProcessResult::Deliver`]: the pre-processor produced a
//!   final message itself; the scheduler routes it through the delivery
//!   channels without invoking the kernel.
//! - [`CronPreProcessResult::RunLlm`]: the pre-processor mutated the
//!   prompt; the scheduler runs the kernel with the new prompt.
//! - [`CronPreProcessResult::Passthrough`]: this pre-processor abstains;
//!   the scheduler tries the next one or, if none chime in, runs the
//!   kernel with the unmodified prompt.

use async_trait::async_trait;
use crabgent_store::records::CronJob;

/// Outcome of a single pre-processor invocation.
#[derive(Debug, Clone)]
pub enum CronPreProcessResult {
    /// Skip this run; do not invoke the kernel or deliveries.
    Skip,
    /// Deliver this text directly through registered deliveries; no kernel run.
    Deliver(String),
    /// Run the kernel with the augmented prompt.
    RunLlm(String),
    /// Abstain; let the next pre-processor (or default flow) decide.
    Passthrough,
}

/// Pre-processor invoked before the cron job reaches the kernel.
#[async_trait]
pub trait CronPreProcessor: Send + Sync {
    /// Inspect `job` and return how the scheduler should proceed.
    async fn pre_process(&self, job: &CronJob) -> CronPreProcessResult;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crabgent_core::MemoryScope;
    use crabgent_store::CronJobId;
    use crabgent_store::records::CronSchedule;
    use serde_json::json;

    fn fixture(prompt: &str) -> CronJob {
        CronJob {
            id: CronJobId::new(),
            name: "demo".into(),
            scope: MemoryScope::default(),
            prompt: prompt.into(),
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

    struct PassPre;
    #[async_trait]
    impl CronPreProcessor for PassPre {
        async fn pre_process(&self, _job: &CronJob) -> CronPreProcessResult {
            CronPreProcessResult::Passthrough
        }
    }

    struct AugmentPre;
    #[async_trait]
    impl CronPreProcessor for AugmentPre {
        async fn pre_process(&self, job: &CronJob) -> CronPreProcessResult {
            CronPreProcessResult::RunLlm(format!("[ctx]\n{}", job.prompt))
        }
    }

    #[tokio::test]
    async fn passthrough_does_not_change_prompt() {
        let p = PassPre;
        let job = fixture("hello");
        assert!(matches!(
            p.pre_process(&job).await,
            CronPreProcessResult::Passthrough
        ));
    }

    #[tokio::test]
    async fn augment_returns_run_llm_with_modified_prompt() {
        let p = AugmentPre;
        let job = fixture("hello");
        match p.pre_process(&job).await {
            CronPreProcessResult::RunLlm(text) => {
                assert!(text.starts_with("[ctx]"));
                assert!(text.contains("hello"));
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[test]
    fn outcome_clone_independent() {
        let a = CronPreProcessResult::Deliver("done".into());
        let b = a.clone();
        match (a, b) {
            (CronPreProcessResult::Deliver(x), CronPreProcessResult::Deliver(y)) => {
                assert_eq!(x, "done");
                assert_eq!(y, "done");
            }
            _ => panic!("expected two Deliver variants"),
        }
    }
}
