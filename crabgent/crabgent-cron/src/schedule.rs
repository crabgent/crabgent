//! Schedule advancement: compute the next fire time of a [`CronSchedule`].
//!
//! Two modes:
//! - Interval (`interval_secs = Some(n)`): next run is `after + n seconds`.
//! - Cron expression (`cron_expr = Some(expr)`, optional `cron_tz`): parsed
//!   via the [`cron`] crate. Five-field POSIX expressions are converted to
//!   the seven-field format the crate expects, including a 0-based to
//!   1-based day-of-week conversion.

use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use crabgent_log::warn;
use crabgent_store::records::CronSchedule;

/// Compute the next fire time strictly after `after` for `schedule`.
/// Returns `None` if the schedule is malformed (invalid cron expr, neither
/// `interval_secs` nor `cron_expr` set).
pub fn next_run(schedule: &CronSchedule, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Some(secs) = schedule.interval_secs {
        let delta = Duration::seconds(i64::try_from(secs).ok()?);
        return after.checked_add_signed(delta);
    }
    let expr = schedule.cron_expr.as_deref()?;
    next_cron_fire(expr, schedule.cron_tz.as_deref(), after)
}

/// Parse a cron expression and return `Ok(())` if valid. Accepts 5-field
/// POSIX (min hour dom month dow), 6-field (with seconds), and 7-field
/// (with year) forms.
pub fn validate_cron_expr(expr: &str) -> Result<(), String> {
    let expr7 = to_cron7(expr);
    cron::Schedule::from_str(&expr7)
        .map(|_| ())
        .map_err(|e| format!("invalid cron expression '{expr}': {e}"))
}

fn next_cron_fire(expr: &str, tz: Option<&str>, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let expr7 = to_cron7(expr);
    let schedule = cron::Schedule::from_str(&expr7).ok()?;

    if let Some(Ok(tz)) = tz.map(str::parse::<chrono_tz::Tz>) {
        let local = after.with_timezone(&tz);
        return schedule
            .after(&local)
            .next()
            .map(|dt| dt.with_timezone(&Utc));
    }
    if let Some(raw_tz) = tz {
        warn!(invalid_tz = raw_tz, "cron: falling back to UTC");
    }
    schedule
        .after(&after)
        .next()
        .map(|dt| dt.with_timezone(&Utc))
}

fn to_cron7(expr: &str) -> String {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    match fields.as_slice() {
        [minute, hour, dom, month, dow_field] => {
            let dow = convert_dow_posix_to_crate(dow_field);
            format!("0 {minute} {hour} {dom} {month} {dow} *")
        }
        [_, _, _, _, _, _] => format!("{expr} *"),
        _ => expr.to_string(),
    }
}

fn convert_dow_posix_to_crate(field: &str) -> String {
    if field == "*" || field == "?" {
        return field.to_string();
    }
    field
        .split(',')
        .map(convert_dow_part)
        .collect::<Vec<_>>()
        .join(",")
}

fn convert_dow_part(part: &str) -> String {
    if part.starts_with("*/") {
        return part.to_string();
    }
    let (range, step) = match part.split_once('/') {
        Some((r, s)) => (r, Some(s)),
        None => (part, None),
    };
    let converted = match range.split_once('-') {
        Some((a, b)) => convert_dow_range(a, b),
        None => posix_dow_val(range),
    };
    step.map_or_else(|| converted.clone(), |s| format!("{converted}/{s}"))
}

fn convert_dow_range(start_raw: &str, end_raw: &str) -> String {
    let start = posix_dow_val(start_raw);
    let end = posix_dow_val(end_raw);
    match (start.parse::<u8>(), end.parse::<u8>()) {
        (Ok(s), Ok(e)) if e < s => format!("{s}-7,1-{e}"),
        _ => format!("{start}-{end}"),
    }
}

fn posix_dow_val(val: &str) -> String {
    val.parse::<u8>()
        .map_or_else(|_| val.to_string(), |n| format!("{}", (n % 7) + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, NaiveDate, TimeZone, Timelike};
    use tracing_test::traced_test;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(y, m, d)
                .expect("test result")
                .and_hms_opt(h, min, 0)
                .expect("test result"),
        )
    }

    #[test]
    fn next_run_interval_60s() {
        let s = CronSchedule::every(60);
        let after = at(2026, 5, 9, 12, 0);
        let n = next_run(&s, after).expect("test result");
        assert_eq!(n - after, Duration::seconds(60));
    }

    #[test]
    fn next_run_interval_1h() {
        let s = CronSchedule::every(3600);
        let after = at(2026, 5, 9, 12, 0);
        assert_eq!(
            next_run(&s, after).expect("test result"),
            at(2026, 5, 9, 13, 0)
        );
    }

    #[test]
    fn next_run_interval_overflow_returns_none() {
        let s = CronSchedule::every(u64::MAX);
        let after = at(2026, 5, 9, 12, 0);
        assert!(next_run(&s, after).is_none());
    }

    #[test]
    fn next_run_cron_daily_9am_utc() {
        let s = CronSchedule::cron("0 9 * * *", None);
        let after = at(2026, 5, 9, 8, 0);
        let n = next_run(&s, after).expect("test result");
        assert_eq!(n.hour(), 9);
        assert_eq!(n.day(), 9);
    }

    #[test]
    fn next_run_cron_daily_9am_berlin() {
        let s = CronSchedule::cron("0 9 * * *", Some("Europe/Berlin".into()));
        let after = at(2026, 5, 9, 5, 0);
        let n = next_run(&s, after).expect("test result");
        // 09:00 Europe/Berlin in May = 07:00 UTC (CEST)
        assert_eq!(n.hour(), 7);
    }

    #[test]
    fn next_run_invalid_cron_returns_none() {
        let s = CronSchedule::cron("not-a-cron", None);
        let after = at(2026, 5, 9, 12, 0);
        assert!(next_run(&s, after).is_none());
    }

    #[traced_test]
    #[test]
    fn next_run_invalid_tz_falls_back_to_utc() {
        let s = CronSchedule::cron("0 9 * * *", Some("Mars/Olympus_Mons".into()));
        let after = at(2026, 5, 9, 5, 0);
        let n = next_run(&s, after).expect("test result");
        // Falls back to UTC: 09:00 UTC same day.
        assert_eq!(n.hour(), 9);
        assert!(logs_contain("cron: falling back to UTC"));
        assert!(logs_contain("invalid_tz"));
    }

    #[test]
    fn next_run_empty_schedule_returns_none() {
        let s = CronSchedule {
            interval_secs: None,
            cron_expr: None,
            cron_tz: None,
        };
        assert!(next_run(&s, at(2026, 5, 9, 12, 0)).is_none());
    }

    #[test]
    fn validate_accepts_valid_5_field() {
        validate_cron_expr("0 9 * * *").expect("test result");
        validate_cron_expr("*/5 * * * *").expect("test result");
        validate_cron_expr("0 9 * * 1-5").expect("test result");
    }

    #[test]
    fn validate_rejects_garbage() {
        assert!(validate_cron_expr("garbage").is_err());
        assert!(validate_cron_expr("").is_err());
    }

    #[test]
    fn validate_accepts_6_and_7_field() {
        validate_cron_expr("0 0 9 * * *").expect("test result");
        validate_cron_expr("0 0 9 * * * *").expect("test result");
    }

    #[test]
    fn dow_posix_to_crate_sunday_0_and_7_align() {
        let s0 = CronSchedule::cron("0 10 * * 0", None);
        let s7 = CronSchedule::cron("0 10 * * 7", None);
        let after = at(2026, 5, 8, 12, 0); // Friday
        let n0 = next_run(&s0, after).expect("test result");
        let n7 = next_run(&s7, after).expect("test result");
        assert_eq!(n0, n7);
        assert_eq!(n0.weekday(), chrono::Weekday::Sun);
    }

    #[test]
    fn dow_range_wraparound_fires_on_sunday() {
        // Fri-Sun 10:00, after Saturday 12:00 must fire on Sunday.
        let s = CronSchedule::cron("0 10 * * 5-7", None);
        let saturday = at(2026, 5, 9, 12, 0);
        let n = next_run(&s, saturday).expect("test result");
        assert_eq!(n.weekday(), chrono::Weekday::Sun);
    }

    #[test]
    fn dow_step_passthrough() {
        // "*/2" in DOW field: every other day.
        validate_cron_expr("0 9 * * */2").expect("test result");
    }

    #[test]
    fn dow_named_days_passthrough() {
        validate_cron_expr("0 9 * * MON-FRI").expect("test result");
    }

    #[test]
    fn dow_list_converts() {
        // Mon,Wed,Fri = POSIX 1,3,5
        validate_cron_expr("0 9 * * 1,3,5").expect("test result");
    }
}
