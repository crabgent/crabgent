//! Schedule validation helpers for cron tool writes.

use chrono::{DateTime, Utc};
use crabgent_core::error::ToolError;
use crabgent_cron::{next_run, validate_cron_expr};
use crabgent_store::records::CronSchedule;

pub fn validate_schedule(schedule: &CronSchedule) -> Result<(), ToolError> {
    match (
        schedule.interval_secs,
        schedule.cron_expr.as_deref(),
        schedule.cron_tz.as_deref(),
    ) {
        (Some(0), _, _) => Err(ToolError::InvalidArgs(
            "schedule.interval_secs must be at least 1".into(),
        )),
        (Some(_), None, None) => Ok(()),
        (Some(_), Some(_), _) => Err(ToolError::InvalidArgs(
            "schedule must set either interval_secs or cron_expr, not both".into(),
        )),
        (None, Some(expr), _) => validate_cron_expr(expr)
            .map_err(|err| ToolError::InvalidArgs(format!("schedule.cron_expr: {err}"))),
        (Some(_) | None, None, Some(_)) => Err(ToolError::InvalidArgs(
            "schedule.cron_tz requires schedule.cron_expr".into(),
        )),
        (None, None, None) => Err(ToolError::InvalidArgs(
            "schedule must set interval_secs or cron_expr".into(),
        )),
    }
}

pub fn first_next_run(
    schedule: &CronSchedule,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, ToolError> {
    next_run(schedule, now).ok_or_else(|| {
        ToolError::InvalidArgs("schedule has no valid next run after validation".into())
    })
}
