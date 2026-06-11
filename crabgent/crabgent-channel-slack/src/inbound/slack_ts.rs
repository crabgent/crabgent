//! Parse Slack `ts` strings into UTC timestamps.

use chrono::{DateTime, Utc};

/// Parse a Slack `ts` string of the form `"<seconds>.<microseconds>"`
/// into a UTC timestamp.
///
/// Slack emits message and event timestamps as floats encoded in a
/// string (e.g. `"1716312345.678901"`). Microsecond precision is
/// preserved when the fractional part is present. Malformed values,
/// negative numbers, and values outside the chrono range return
/// `None` so callers can fall back gracefully without panicking on
/// attacker-controlled wire data.
#[must_use]
pub fn parse_slack_ts(ts: &str) -> Option<DateTime<Utc>> {
    let trimmed = ts.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (secs_part, micros_part) = match trimmed.split_once('.') {
        Some((s, m)) => (s, m),
        None => (trimmed, ""),
    };
    let secs: i64 = secs_part.parse().ok()?;
    if secs < 0 {
        return None;
    }
    let micros: u32 = if micros_part.is_empty() {
        0
    } else {
        let padded: String = micros_part
            .chars()
            .chain(std::iter::repeat('0'))
            .take(6)
            .collect();
        padded.parse().ok()?
    };
    let nanos = micros.checked_mul(1_000)?;
    DateTime::<Utc>::from_timestamp(secs, nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_with_microseconds() {
        let parsed = parse_slack_ts("1716312345.678901").expect("parse");
        assert_eq!(parsed.timestamp(), 1_716_312_345);
        assert_eq!(parsed.timestamp_subsec_micros(), 678_901);
    }

    #[test]
    fn seconds_only_without_dot() {
        let parsed = parse_slack_ts("1716312345").expect("parse");
        assert_eq!(parsed.timestamp(), 1_716_312_345);
        assert_eq!(parsed.timestamp_subsec_nanos(), 0);
    }

    #[test]
    fn empty_returns_none() {
        assert!(parse_slack_ts("").is_none());
        assert!(parse_slack_ts("   ").is_none());
    }

    #[test]
    fn non_numeric_returns_none() {
        assert!(parse_slack_ts("not_a_ts").is_none());
        assert!(parse_slack_ts("abc.def").is_none());
        assert!(parse_slack_ts("12.3.4").is_none());
    }

    #[test]
    fn negative_returns_none() {
        assert!(parse_slack_ts("-1.0").is_none());
    }
}
