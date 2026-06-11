//! Cron executor for memory consolidation.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::Subject;
use crabgent_cron::{CronError, CronExecCtx, CronExecResult, CronExecutor};
use crabgent_store::CronJob;

use crate::{ConsolidationError, ConsolidationRunner};

pub type SubjectResolver =
    Arc<dyn Fn(&CronJob) -> Result<Subject, ConsolidationError> + Send + Sync>;

pub struct ConsolidationCronExecutor {
    runner: Arc<ConsolidationRunner>,
    subject_resolver: SubjectResolver,
}

impl ConsolidationCronExecutor {
    pub fn new(runner: Arc<ConsolidationRunner>, subject_resolver: SubjectResolver) -> Self {
        Self {
            runner,
            subject_resolver,
        }
    }
}

impl From<ConsolidationError> for CronError {
    fn from(value: ConsolidationError) -> Self {
        Self::scheduler(value)
    }
}

#[async_trait]
impl CronExecutor for ConsolidationCronExecutor {
    async fn execute(&self, ctx: CronExecCtx<'_>) -> Result<CronExecResult, CronError> {
        let subject = (self.subject_resolver)(ctx.job)?;
        let result = self
            .runner
            .run(&subject, ctx.job.scope.clone(), ctx.cancel)
            .await;
        match result {
            Ok(result) => Ok(CronExecResult {
                final_text: Some(format!(
                    "memory consolidation processed {} sessions",
                    result.sessions_processed
                )),
                error: None,
            }),
            Err(err) => Ok(CronExecResult {
                final_text: None,
                error: Some(err.to_string()),
            }),
        }
    }
}
