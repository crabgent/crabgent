//! Retry helpers for OpenAI-compatible HTTP calls.

use std::time::Duration;

use crabgent_provider_transport::{
    DEFAULT_MAX_BACKOFF, capped_backoff, longer_retry_delay,
    parse_retry_after as parse_retry_after_header,
};
use reqwest::header::HeaderMap;

/// Whether an HTTP status should be retried by the provider.
pub const fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500..=599)
}

/// Parse `Retry-After` in seconds and cap excessive server delays.
pub fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    parse_retry_after_header(headers)
}

/// Exponential backoff, capped to keep retries bounded.
pub fn retry_delay(attempt: u32, base: Duration) -> Duration {
    capped_backoff(attempt, base, DEFAULT_MAX_BACKOFF)
}

/// Pick the longer server-provided delay or local backoff delay.
pub fn sleep_delay(attempt: u32, base: Duration, server: Option<Duration>) -> Duration {
    longer_retry_delay(attempt, base, server, DEFAULT_MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn retryable_statuses_match_openai_policy() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(599));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(401));
    }

    #[test]
    fn retry_after_parses_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("7"));

        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(7)));
    }

    #[test]
    fn sleep_delay_uses_larger_delay() {
        let base = Duration::from_millis(100);
        let server = Duration::from_secs(3);

        assert_eq!(sleep_delay(0, base, Some(server)), server);
        assert_eq!(sleep_delay(1, base, None), Duration::from_millis(200));
    }
}
