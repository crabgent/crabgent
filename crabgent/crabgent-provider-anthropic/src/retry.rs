//! Retry helpers: exponential backoff with jitter, retry-after parsing,
//! retryable status classification.

use std::time::Duration;

use crabgent_provider_transport::{
    DEFAULT_MAX_BACKOFF, capped_backoff_with_jitter, longer_retry_delay_with_jitter,
    parse_retry_after as parse_retry_after_header,
};
use reqwest::header::HeaderMap;

/// Exponential backoff: `base * 2^attempt + jitter`, capped at 30s.
#[must_use]
pub fn retry_delay(attempt: u32, base: Duration) -> Duration {
    capped_backoff_with_jitter(attempt, base, DEFAULT_MAX_BACKOFF)
}

/// Parse the `Retry-After` HTTP header (in seconds, capped at 300s).
#[must_use]
pub fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    parse_retry_after_header(headers)
}

/// Pick the longer of a server-supplied retry-after vs computed backoff.
#[must_use]
pub fn retry_sleep_delay(attempt: u32, base: Duration, server: Option<Duration>) -> Duration {
    longer_retry_delay_with_jitter(attempt, base, server, DEFAULT_MAX_BACKOFF)
}

/// Whether an HTTP status code triggers retry (429, 500, 529).
#[must_use]
pub const fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 529)
}

/// Retryable Anthropic SSE error classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryableStreamErrorKind {
    Overloaded,
    Api,
}

/// Whether an SSE-level stop reason indicates a retryable stream error.
#[must_use]
pub fn is_retryable_stream_error(stop_reason: &str) -> Option<RetryableStreamErrorKind> {
    match stop_reason {
        "error:overloaded_error" => Some(RetryableStreamErrorKind::Overloaded),
        "error:api_error" => Some(RetryableStreamErrorKind::Api),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn delay_grows_then_caps_at_30s() {
        let base = Duration::from_millis(500);
        let d0 = retry_delay(0, base);
        let d10 = retry_delay(10, base);
        assert!(d0 >= base && d0 <= base + base / 4 + Duration::from_secs(1));
        // 500ms * 2^10 = 512s; capped at 30s + jitter (max 7.5s)
        assert!(d10 <= Duration::from_secs(38));
        assert!(d10 >= Duration::from_secs(30));
    }

    #[test]
    fn parse_retry_after_seconds() {
        let mut h = HeaderMap::new();
        h.insert("retry-after", HeaderValue::from_static("12"));
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(12)));
    }

    #[test]
    fn parse_retry_after_caps_at_300s() {
        let mut h = HeaderMap::new();
        h.insert("retry-after", HeaderValue::from_static("99999"));
        assert_eq!(parse_retry_after(&h), Some(Duration::from_mins(5)));
    }

    #[test]
    fn parse_retry_after_missing_returns_none() {
        let h = HeaderMap::new();
        assert_eq!(parse_retry_after(&h), None);
    }

    #[test]
    fn parse_retry_after_invalid_returns_none() {
        let mut h = HeaderMap::new();
        h.insert("retry-after", HeaderValue::from_static("nope"));
        assert_eq!(parse_retry_after(&h), None);
    }

    #[test]
    fn retry_sleep_takes_longer_of_server_or_backoff() {
        let base = Duration::from_millis(100);
        let big = Duration::from_mins(1);
        assert_eq!(retry_sleep_delay(0, base, Some(big)), big);
        let calc = retry_sleep_delay(0, base, None);
        assert!(calc >= base);
    }

    #[test]
    fn retryable_status_classification() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(529));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(503));
    }

    #[test]
    fn retryable_stream_errors() {
        assert_eq!(
            is_retryable_stream_error("error:overloaded_error"),
            Some(RetryableStreamErrorKind::Overloaded)
        );
        assert_eq!(
            is_retryable_stream_error("error:api_error"),
            Some(RetryableStreamErrorKind::Api)
        );
        assert_eq!(is_retryable_stream_error("end_turn"), None);
        assert_eq!(is_retryable_stream_error("error:something_else"), None);
    }
}
