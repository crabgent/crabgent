//! `AgentDispatchCronExecutor`: routes each cron job to the kernel +
//! `KernelCronExecutor` of the agent named in `job.scope.agent`.
//!
//! The upstream `CronScheduler` holds a single `Arc<Kernel>` and passes it
//! into every executor call. Without per-agent dispatch, every cron job
//! (regardless of `scope.agent`) would execute against the first agent's
//! kernel — wrong identity, wrong system prompt, wrong channel-bot.
//!
//! This wrapper looks up the right kernel + per-agent
//! `KernelCronExecutor` (with that agent's `system_prompt` and `model`),
//! substitutes them into a fresh `CronExecCtx`, and delegates the actual
//! `kernel.run(...)` call to the inner executor.
//!
//! A cron job with no `scope.agent` or an unknown agent name is an error,
//! NOT silently re-routed to some "default" agent: cron identity drives
//! channel-bot, system-prompt and tool access, so a misrouted job would
//! deliver as the wrong identity. The job is reported as a failed run.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::Kernel;
use crabgent_cron::{
    CronDelivery, CronError, CronExecCtx, CronExecResult, CronExecutor, KernelCronExecutor,
};
use crabgent_log::warn;

pub struct AgentDispatchCronExecutor {
    per_agent: HashMap<String, (Arc<Kernel>, Arc<KernelCronExecutor>)>,
}

impl AgentDispatchCronExecutor {
    pub fn new() -> Self {
        Self {
            per_agent: HashMap::new(),
        }
    }

    pub fn insert(
        &mut self,
        agent: impl Into<String>,
        kernel: Arc<Kernel>,
        exec: Arc<KernelCronExecutor>,
    ) {
        self.per_agent.insert(agent.into(), (kernel, exec));
    }
}

#[async_trait]
impl CronExecutor for AgentDispatchCronExecutor {
    async fn execute(&self, ctx: CronExecCtx<'_>) -> Result<CronExecResult, CronError> {
        let Some(agent) = ctx.job.scope.agent.as_deref() else {
            return Ok(CronExecResult {
                final_text: None,
                error: Some(format!(
                    "cron job {:?} ({}) has no scope.agent set; refusing to run \
                     without an explicit agent identity",
                    ctx.job.name, ctx.job.id
                )),
            });
        };
        let Some((kernel, exec)) = self.per_agent.get(agent).cloned() else {
            return Ok(CronExecResult {
                final_text: None,
                error: Some(format!(
                    "cron job {:?} ({}) scope.agent={agent:?} is not configured in \
                     this runtime; available: {:?}",
                    ctx.job.name,
                    ctx.job.id,
                    self.per_agent.keys().collect::<Vec<_>>(),
                )),
            });
        };
        let new_ctx = CronExecCtx {
            job: ctx.job,
            kernel,
            prompt: ctx.prompt,
            cancel: ctx.cancel,
        };
        exec.execute(new_ctx).await
    }
}

pub struct ErrorDeliveringCronExecutor {
    inner: Arc<dyn CronExecutor>,
    delivery: Arc<dyn CronDelivery>,
}

impl ErrorDeliveringCronExecutor {
    pub fn new(inner: Arc<dyn CronExecutor>, delivery: Arc<dyn CronDelivery>) -> Self {
        Self { inner, delivery }
    }
}

#[async_trait]
impl CronExecutor for ErrorDeliveringCronExecutor {
    async fn execute(&self, ctx: CronExecCtx<'_>) -> Result<CronExecResult, CronError> {
        let job = ctx.job;
        let result = self.inner.execute(ctx).await;
        match &result {
            Ok(run) if run.final_text.is_none() => {
                if let Some(error) = run.error.as_deref() {
                    self.deliver_error(job, error).await;
                }
            }
            Err(error) => self.deliver_error(job, &error.to_string()).await,
            _ => {}
        }
        result
    }
}

impl ErrorDeliveringCronExecutor {
    async fn deliver_error(&self, job: &crabgent_store::records::CronJob, error: &str) {
        let message = format!("Cron \"{}\" fehlgeschlagen: {error}", job.name);
        if let Err(err) = self.delivery.deliver(job, &message).await {
            warn!(
                job = %job.name,
                error = %err,
                "cron error delivery failed"
            );
        }
    }
}
