//! Pure formatting helpers for time hints.

use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};

use crate::config::TimeHintConfig;
use crate::hook::{TIME_GUIDANCE, TIME_HINT_CLOSE, TIME_HINT_OPEN};
use crate::provider::HolidayProvider;

/// Discrete buckets the LLM can anchor on instead of reasoning about
/// raw deltas. Boundaries are chosen so a typical reply round-trip
/// stays in `active`, while gaps that humans would naturally treat as
/// pauses move into `fresh`, `long_pause`, and `gap_day`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseMarker {
    Active,
    Fresh,
    LongPause,
    GapDay,
}

impl PauseMarker {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Fresh => "fresh",
            Self::LongPause => "long-pause",
            Self::GapDay => "gap-day",
        }
    }

    #[must_use]
    pub fn classify(delta: Duration) -> Self {
        if delta < Duration::minutes(5) {
            Self::Active
        } else if delta < Duration::hours(2) {
            Self::Fresh
        } else if delta < Duration::hours(24) {
            Self::LongPause
        } else {
            Self::GapDay
        }
    }
}

pub fn build_hint<P: HolidayProvider>(
    now_utc: DateTime<Utc>,
    config: &TimeHintConfig,
    provider: &P,
    last_user_ts: Option<DateTime<Utc>>,
) -> String {
    let now = now_utc.with_timezone(&config.timezone);
    let today = now.date_naive();
    let week = now.iso_week().week();
    let day_type = if now.weekday().num_days_from_monday() < 5 {
        "workday"
    } else {
        "weekend"
    };

    let mut hint = format!(
        "{TIME_HINT_OPEN}\n\
         Current date/time: {} ({})\n\
         Calendar week {week}, {day_type}. Time of day: {}.\n\
         {}",
        now.format("%A, %Y-%m-%d %H:%M"),
        config.timezone.name(),
        time_of_day_label(now.hour()),
        format_calendar_anchors(today, week),
    );

    if let Some(context) = format_holiday_context(today, config, provider) {
        hint.push('\n');
        hint.push_str(&context);
    }

    if let Some(last) = last_user_ts {
        hint.push('\n');
        hint.push_str(&format_recency_block(now_utc, last, config));
    }

    hint.push_str("\n\n");
    hint.push_str(TIME_GUIDANCE);
    hint.push('\n');
    hint.push_str(TIME_HINT_CLOSE);
    hint
}

/// Compose the two `Last user message: ...` and `Pause: ...` lines
/// that the LLM uses to honor temporal distance between turns.
pub fn format_recency_block(
    now_utc: DateTime<Utc>,
    last_user_ts: DateTime<Utc>,
    config: &TimeHintConfig,
) -> String {
    let local = last_user_ts.with_timezone(&config.timezone);
    let delta = now_utc.signed_duration_since(last_user_ts);
    // A future-timestamp is non-physical but possible with clock skew;
    // clamp to zero so the rendered delta stays meaningful instead of
    // showing "-3h ago".
    let delta = if delta < Duration::zero() {
        Duration::zero()
    } else {
        delta
    };
    let marker = PauseMarker::classify(delta);
    format!(
        "Last user message: {} ({}, {}) ({}).\n\
         Pause: {}.",
        local.format("%Y-%m-%d %H:%M"),
        local.format("%A"),
        time_of_day_label(local.hour()),
        format_duration(delta),
        marker.as_str(),
    )
}

/// Human-friendly rendering of a non-negative `Duration`. Caps at the
/// largest unit that fits naturally so the LLM does not parse mixed
/// scales like `"3d 0h 0m 0s"`.
pub fn format_duration(delta: Duration) -> String {
    let secs = delta.num_seconds();
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        let rem_mins = mins % 60;
        if rem_mins == 0 {
            return format!("{hours}h ago");
        }
        return format!("{hours}h {rem_mins}m ago");
    }
    let days = hours / 24;
    let rem_hours = hours % 24;
    if rem_hours == 0 {
        format!("{days}d ago")
    } else {
        format!("{days}d {rem_hours}h ago")
    }
}

pub const fn time_of_day_label(hour: u32) -> &'static str {
    match hour {
        6..=11 => "morning",
        12..=13 => "midday",
        14..=17 => "afternoon",
        18..=21 => "evening",
        _ => "night",
    }
}

pub fn format_calendar_anchors(today: NaiveDate, week: u32) -> String {
    // invariant: `today` comes from a realistic clock; +/- a few days stays
    // inside chrono's representable range (panics only near year +/-262143),
    // so the fallback to `today`/`week_start` is unreachable in practice.
    let yesterday = today.pred_opt().unwrap_or(today);
    let tomorrow = today.succ_opt().unwrap_or(today);
    let week_start = today
        .checked_sub_signed(Duration::days(i64::from(
            today.weekday().num_days_from_monday(),
        )))
        .unwrap_or(today);
    let next_week_start = week_start
        .checked_add_signed(Duration::days(7))
        .unwrap_or(week_start);

    format!(
        "Today: {} ({}, KW {week:02})\n\
         Yesterday: {} ({})\n\
         Tomorrow: {} ({})\n\
         This week: {}\n\
         Next week: {}",
        today.format("%Y-%m-%d"),
        today.format("%A"),
        yesterday.format("%Y-%m-%d"),
        yesterday.format("%A"),
        tomorrow.format("%Y-%m-%d"),
        tomorrow.format("%A"),
        format_week_anchor(week_start),
        format_week_anchor(next_week_start),
    )
}

pub fn format_holiday_context<P: HolidayProvider>(
    today: NaiveDate,
    config: &TimeHintConfig,
    provider: &P,
) -> Option<String> {
    let mut lines = Vec::new();

    if let Some(name) = provider.get_holiday(today, &config.country, &config.subdivision) {
        lines.push(format!("Today is a public holiday: {name}."));
    }

    if let Some(tomorrow) = today.succ_opt()
        && let Some(name) = provider.get_holiday(tomorrow, &config.country, &config.subdivision)
    {
        lines.push(format!("Tomorrow is a public holiday: {name}."));
    }

    // invariant: skipping today + tomorrow stays inside chrono's date range
    // for any realistic clock; fallback to `today` is unreachable in practice.
    let skip_until = today.succ_opt().and_then(|t| t.succ_opt()).unwrap_or(today);
    let upcoming = provider.upcoming_holidays(
        skip_until,
        &config.country,
        &config.subdivision,
        config.upcoming_count,
    );
    if !upcoming.is_empty() {
        let items: Vec<String> = upcoming
            .iter()
            .map(|(date, name)| format!("{name} ({})", date.format("%A %d.%m.")))
            .collect();
        lines.push(format!(
            "Upcoming holidays ({}/{}): {}.",
            config.country,
            config.subdivision,
            items.join(", ")
        ));
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn format_week_anchor(week_start: NaiveDate) -> String {
    [("Mon", 0), ("Tue", 1), ("Wed", 2), ("Thu", 3), ("Fri", 4)]
        .into_iter()
        .map(|(label, offset)| {
            // invariant: 0..=4 days from a realistic week start stays inside
            // chrono's date range; fallback to `week_start` is unreachable.
            let date = week_start
                .checked_add_signed(Duration::days(offset))
                .unwrap_or(week_start);
            format!("{label} {}", date.format("%Y-%m-%d"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::provider::EmbeddedHolidayProvider;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("valid date in test")
    }

    #[test]
    fn build_hint_uses_time_tag() {
        let now = DateTime::parse_from_rfc3339("2026-05-21T14:32:11Z")
            .expect("rfc3339")
            .with_timezone(&Utc);
        let provider = EmbeddedHolidayProvider::new();

        let hint = build_hint(now, &TimeHintConfig::default(), &provider, None);

        assert!(hint.starts_with(&format!("{TIME_HINT_OPEN}\n")));
        assert!(hint.ends_with(&format!("\n{TIME_HINT_CLOSE}")));
        // The injected open tag carries the trust-fence sentinel.
        assert!(hint.starts_with("<time crabgent=\"1\">"));
        assert!(!hint.contains("<!-- crabgent-calendar-time-hint -->"));
    }

    #[test]
    fn formats_weekday_anchors_for_iso_week() {
        let anchors = format_calendar_anchors(d(2026, 5, 6), 19);

        assert_eq!(
            anchors,
            concat!(
                "Today: 2026-05-06 (Wednesday, KW 19)\n",
                "Yesterday: 2026-05-05 (Tuesday)\n",
                "Tomorrow: 2026-05-07 (Thursday)\n",
                "This week: Mon 2026-05-04, Tue 2026-05-05, Wed 2026-05-06, ",
                "Thu 2026-05-07, Fri 2026-05-08\n",
                "Next week: Mon 2026-05-11, Tue 2026-05-12, Wed 2026-05-13, ",
                "Thu 2026-05-14, Fri 2026-05-15",
            )
        );
    }

    #[test]
    fn time_of_day_labels_follow_spec() {
        assert_eq!(time_of_day_label(5), "night");
        assert_eq!(time_of_day_label(6), "morning");
        assert_eq!(time_of_day_label(12), "midday");
        assert_eq!(time_of_day_label(14), "afternoon");
        assert_eq!(time_of_day_label(18), "evening");
        assert_eq!(time_of_day_label(22), "night");
    }

    #[test]
    fn pause_marker_classify_covers_all_buckets() {
        assert_eq!(
            PauseMarker::classify(Duration::seconds(30)),
            PauseMarker::Active
        );
        assert_eq!(
            PauseMarker::classify(Duration::minutes(4)),
            PauseMarker::Active
        );
        assert_eq!(
            PauseMarker::classify(Duration::minutes(5)),
            PauseMarker::Fresh
        );
        assert_eq!(
            PauseMarker::classify(Duration::hours(1)),
            PauseMarker::Fresh
        );
        assert_eq!(
            PauseMarker::classify(Duration::hours(2)),
            PauseMarker::LongPause
        );
        assert_eq!(
            PauseMarker::classify(Duration::hours(23)),
            PauseMarker::LongPause
        );
        assert_eq!(
            PauseMarker::classify(Duration::hours(24)),
            PauseMarker::GapDay
        );
        assert_eq!(
            PauseMarker::classify(Duration::days(3)),
            PauseMarker::GapDay
        );
    }

    #[test]
    fn format_duration_picks_natural_unit() {
        assert_eq!(format_duration(Duration::seconds(45)), "45s ago");
        assert_eq!(format_duration(Duration::minutes(30)), "30m ago");
        assert_eq!(format_duration(Duration::hours(3)), "3h ago");
        assert_eq!(
            format_duration(Duration::hours(3) + Duration::minutes(12)),
            "3h 12m ago"
        );
        assert_eq!(format_duration(Duration::days(2)), "2d ago");
        assert_eq!(
            format_duration(Duration::days(2) + Duration::hours(5)),
            "2d 5h ago"
        );
    }

    #[test]
    fn format_recency_block_contains_marker_and_ago_clause() {
        let now = DateTime::parse_from_rfc3339("2026-05-21T14:32:11Z")
            .expect("rfc3339")
            .with_timezone(&Utc);
        let last = DateTime::parse_from_rfc3339("2026-05-21T11:00:00Z")
            .expect("rfc3339")
            .with_timezone(&Utc);
        let block = format_recency_block(now, last, &TimeHintConfig::default());
        assert!(block.contains("Last user message: 2026-05-21 13:00"));
        assert!(block.contains("3h 32m ago"));
        assert!(block.contains("Pause: long-pause."));
    }

    #[test]
    fn format_recency_block_clamps_future_timestamps() {
        let now = DateTime::parse_from_rfc3339("2026-05-21T10:00:00Z")
            .expect("rfc3339")
            .with_timezone(&Utc);
        let last = DateTime::parse_from_rfc3339("2026-05-21T11:00:00Z")
            .expect("rfc3339")
            .with_timezone(&Utc);
        let block = format_recency_block(now, last, &TimeHintConfig::default());
        assert!(block.contains("0s ago"));
        assert!(block.contains("Pause: active."));
    }
}
