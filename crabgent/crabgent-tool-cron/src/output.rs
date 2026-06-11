//! JSON projection helpers for cron tool responses.

use chrono::{DateTime, Utc};
use crabgent_core::ReasoningEffort;
use crabgent_store::records::CronJob;
use serde_json::{Value, json};

pub fn job_to_json(job: &CronJob) -> Value {
    json!({
        "id": job.id.to_string(),
        "name": &job.name,
        "scope": &job.scope,
        "prompt": &job.prompt,
        "schedule": &job.schedule,
        "enabled": job.enabled,
        "run_once": job.run_once,
        "model_override": &job.model_override,
        "reasoning_effort_override": job.reasoning_effort_override.map(ReasoningEffort::as_str),
        "pre_command": &job.pre_command,
        "delivery_ctx": &job.delivery_ctx,
        "last_run": opt_time(job.last_run.as_ref()),
        "next_run": job.next_run.to_rfc3339(),
        "created_at": job.created_at.to_rfc3339(),
        "claimed_at": opt_time(job.claimed_at.as_ref()),
    })
}

fn opt_time(value: Option<&DateTime<Utc>>) -> Option<String> {
    value.map(DateTime::to_rfc3339)
}
