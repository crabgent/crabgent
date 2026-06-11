//! Provider-neutral HTTP transport mechanics.
//!
//! This crate deliberately does not know provider names, auth semantics, wire
//! formats, parser state, or provider/core error types. Provider crates map the
//! neutral errors back into their local public surfaces.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::{Bytes, BytesMut};
use futures::StreamExt as _;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

pub const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(30);
pub const DEFAULT_MAX_RETRY_AFTER: Duration = Duration::from_mins(5);

/// Connect-phase timeout for provider HTTP clients. Bounds DNS plus TCP plus
/// TLS handshake without limiting the response body, which may stream for
/// minutes on completion endpoints.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Idle read timeout between body chunks. Bounds a stalled connection that
/// stops sending without closing, while still permitting long-lived streams
/// that keep producing data. This is per-read, not a total-request cap.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_mins(2);

/// Build a hardened `reqwest` client builder for provider HTTP traffic.
///
/// Disables redirect following (`Policy::none`) so a redirect from an API
/// host cannot silently retarget an authenticated request, and sets a
/// connect-phase plus idle-read timeout. It deliberately sets no total
/// `timeout`: provider completion responses stream for minutes and a total
/// cap would abort healthy long streams. Callers may layer additional
/// configuration on the returned builder before calling `build`.
pub fn hardened_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .read_timeout(DEFAULT_READ_TIMEOUT)
}

/// Build a hardened `reqwest` client with the [`hardened_client_builder`]
/// defaults.
///
/// The builder carries no TLS, proxy, or resolver overrides, so the
/// underlying `build` cannot fail in practice; the `expect` documents that
/// invariant. Callers that need fallible construction or custom builder
/// options should use [`hardened_client_builder`] and call `build`
/// themselves.
#[must_use]
pub fn hardened_client() -> reqwest::Client {
    hardened_client_builder()
        .build()
        .expect("hardened reqwest client builder has no fallible configuration")
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    #[error("cancelled")]
    Cancelled,
    #[error("timeout")]
    Timeout,
    #[error("request error: {0}")]
    Request(#[source] reqwest::Error),
    #[error("body exceeded max size: {max_bytes} bytes")]
    BodyTooLarge { max_bytes: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorBodyMode {
    Text,
    Drain,
}

#[derive(Debug, Clone, Copy)]
pub struct RetryLifecycleConfig {
    pub max_retries: u32,
    pub request_timeout: Duration,
    pub error_body_max_bytes: usize,
    pub error_body_mode: ErrorBodyMode,
}

#[derive(Debug)]
pub struct HttpStatusError {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub body: Option<String>,
}

#[derive(Debug)]
pub enum RetryLifecycleOutcome {
    Ok(reqwest::Response),
    HttpError(HttpStatusError),
    Network(reqwest::Error),
    Timeout,
}

#[must_use]
pub const fn is_auth_status(status: u16) -> bool {
    matches!(status, 401 | 403)
}

#[must_use]
pub fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    parse_retry_after_capped(headers, DEFAULT_MAX_RETRY_AFTER)
}

#[must_use]
pub fn parse_retry_after_capped(headers: &HeaderMap, max: Duration) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|secs| Duration::from_secs(secs).min(max))
}

#[must_use]
pub fn capped_backoff(attempt: u32, base: Duration, max: Duration) -> Duration {
    base.saturating_mul(2_u32.saturating_pow(attempt)).min(max)
}

#[must_use]
pub fn capped_backoff_with_jitter(attempt: u32, base: Duration, max: Duration) -> Duration {
    let capped = capped_backoff(attempt, base, max);
    capped.saturating_add(system_jitter(capped))
}

#[must_use]
pub fn longer_retry_delay(
    attempt: u32,
    base: Duration,
    server: Option<Duration>,
    max_backoff: Duration,
) -> Duration {
    let local = capped_backoff(attempt, base, max_backoff);
    server.map_or(local, |delay| delay.max(local))
}

#[must_use]
pub fn longer_retry_delay_with_jitter(
    attempt: u32,
    base: Duration,
    server: Option<Duration>,
    max_backoff: Duration,
) -> Duration {
    let local = capped_backoff_with_jitter(attempt, base, max_backoff);
    server.map_or(local, |delay| delay.max(local))
}

fn system_jitter(base: Duration) -> Duration {
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| u64::from(duration.subsec_nanos()));
    let quarter = base / 4;
    let quarter_ns = u64::try_from(quarter.as_nanos()).unwrap_or(u64::MAX);
    let modulus = quarter_ns.max(1);
    Duration::from_nanos(now_ns % modulus)
}

#[must_use]
pub fn join_url_path(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

#[must_use]
pub fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub fn check_cancelled(token: &CancellationToken) -> Result<(), TransportError> {
    if token.is_cancelled() {
        Err(TransportError::Cancelled)
    } else {
        Ok(())
    }
}

pub async fn sleep_or_cancel(
    delay: Duration,
    token: &CancellationToken,
) -> Result<(), TransportError> {
    tokio::select! {
        biased;
        () = token.cancelled() => Err(TransportError::Cancelled),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

pub async fn send_with_timeout(
    request: reqwest::RequestBuilder,
    cancel: Option<&CancellationToken>,
    timeout: Duration,
) -> Result<reqwest::Response, TransportError> {
    let noop = CancellationToken::new();
    send_with_timeout_token(request, cancel.unwrap_or(&noop), timeout).await
}

pub async fn send_with_timeout_token(
    request: reqwest::RequestBuilder,
    token: &CancellationToken,
    timeout: Duration,
) -> Result<reqwest::Response, TransportError> {
    tokio::select! {
        biased;
        () = token.cancelled() => Err(TransportError::Cancelled),
        result = tokio::time::timeout(timeout, request.send()) => match result {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) if error.is_timeout() => Err(TransportError::Timeout),
            Ok(Err(error)) => Err(TransportError::Request(error)),
            Err(_) => Err(TransportError::Timeout),
        },
    }
}

pub async fn send_with_retry_lifecycle<B, R, D>(
    mut build_request: B,
    token: &CancellationToken,
    config: RetryLifecycleConfig,
    mut is_retryable_status: R,
    mut retry_delay: D,
) -> Result<RetryLifecycleOutcome, TransportError>
where
    B: FnMut() -> reqwest::RequestBuilder,
    R: FnMut(u16) -> bool,
    D: FnMut(u32, Option<Duration>) -> Duration,
{
    for attempt in 0..=config.max_retries {
        check_cancelled(token)?;
        match send_attempt(build_request(), token, config).await? {
            RetryLifecycleOutcome::Ok(response) => return Ok(RetryLifecycleOutcome::Ok(response)),
            RetryLifecycleOutcome::HttpError(error) => {
                if !is_retryable_status(error.status) || attempt >= config.max_retries {
                    return Ok(RetryLifecycleOutcome::HttpError(error));
                }
                sleep_or_cancel(retry_delay(attempt, error.retry_after), token).await?;
            }
            RetryLifecycleOutcome::Network(error) => {
                if attempt >= config.max_retries {
                    return Ok(RetryLifecycleOutcome::Network(error));
                }
                sleep_or_cancel(retry_delay(attempt, None), token).await?;
            }
            RetryLifecycleOutcome::Timeout => return Ok(RetryLifecycleOutcome::Timeout),
        }
    }
    Ok(RetryLifecycleOutcome::Timeout)
}

async fn send_attempt(
    request: reqwest::RequestBuilder,
    token: &CancellationToken,
    config: RetryLifecycleConfig,
) -> Result<RetryLifecycleOutcome, TransportError> {
    match send_with_timeout_token(request, token, config.request_timeout).await {
        Ok(response) if response.status().is_success() => Ok(RetryLifecycleOutcome::Ok(response)),
        Ok(response) => status_error(response, token, config).await,
        Err(TransportError::Timeout) => Ok(RetryLifecycleOutcome::Timeout),
        Err(TransportError::Request(error)) if error.is_connect() || error.is_request() => {
            Ok(RetryLifecycleOutcome::Network(error))
        }
        Err(error) => Err(error),
    }
}

async fn status_error(
    response: reqwest::Response,
    token: &CancellationToken,
    config: RetryLifecycleConfig,
) -> Result<RetryLifecycleOutcome, TransportError> {
    let status = response.status().as_u16();
    let retry_after = parse_retry_after(response.headers());
    let body = match config.error_body_mode {
        ErrorBodyMode::Text => match read_text_body(
            response,
            Some(token),
            config.request_timeout,
            config.error_body_max_bytes,
        )
        .await
        {
            Ok(body) => Some(body),
            Err(TransportError::Cancelled) => return Err(TransportError::Cancelled),
            Err(_) => None,
        },
        ErrorBodyMode::Drain => {
            if matches!(
                read_body(
                    response,
                    Some(token),
                    config.request_timeout,
                    config.error_body_max_bytes,
                )
                .await,
                Err(TransportError::Cancelled)
            ) {
                return Err(TransportError::Cancelled);
            }
            None
        }
    };
    Ok(RetryLifecycleOutcome::HttpError(HttpStatusError {
        status,
        retry_after,
        body,
    }))
}

pub async fn read_body(
    response: reqwest::Response,
    cancel: Option<&CancellationToken>,
    timeout: Duration,
    max_bytes: usize,
) -> Result<Bytes, TransportError> {
    let noop = CancellationToken::new();
    read_body_with_token(response, cancel.unwrap_or(&noop), timeout, max_bytes).await
}

pub async fn read_body_with_token(
    response: reqwest::Response,
    token: &CancellationToken,
    timeout: Duration,
    max_bytes: usize,
) -> Result<Bytes, TransportError> {
    let read = async {
        let mut out = BytesMut::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                if error.is_timeout() {
                    TransportError::Timeout
                } else {
                    TransportError::Request(error)
                }
            })?;
            if chunk.len() > max_bytes.saturating_sub(out.len()) {
                return Err(TransportError::BodyTooLarge { max_bytes });
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out.freeze())
    };
    tokio::select! {
        biased;
        () = token.cancelled() => Err(TransportError::Cancelled),
        result = tokio::time::timeout(timeout, read) => match result {
            Ok(result) => result,
            Err(_) => Err(TransportError::Timeout),
        },
    }
}

pub async fn read_text_body(
    response: reqwest::Response,
    cancel: Option<&CancellationToken>,
    timeout: Duration,
    max_bytes: usize,
) -> Result<String, TransportError> {
    let bytes = read_body(response, cancel, timeout, max_bytes).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub async fn drain_error_body(
    response: reqwest::Response,
    cancel: Option<&CancellationToken>,
    timeout: Duration,
    max_bytes: usize,
) -> Result<(), TransportError> {
    read_body(response, cancel, timeout, max_bytes)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn retry_after_parses_and_caps_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("99999"));

        assert_eq!(parse_retry_after(&headers), Some(DEFAULT_MAX_RETRY_AFTER));
    }

    #[test]
    fn retry_after_ignores_invalid_values() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("not-seconds"));

        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn backoff_caps_and_server_delay_can_win() {
        let base = Duration::from_millis(100);

        assert_eq!(
            capped_backoff(10, base, Duration::from_secs(1)),
            Duration::from_secs(1)
        );
        assert_eq!(
            longer_retry_delay(1, base, Some(Duration::from_secs(3)), DEFAULT_MAX_BACKOFF),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn jitter_stays_inside_quarter_window() {
        let base = Duration::from_millis(100);
        let delay = capped_backoff_with_jitter(0, base, DEFAULT_MAX_BACKOFF);

        assert!(delay >= base);
        assert!(delay <= base + base / 4);
    }

    #[test]
    fn auth_status_matches_provider_auth_codes() {
        assert!(is_auth_status(401));
        assert!(is_auth_status(403));
        assert!(!is_auth_status(400));
        assert!(!is_auth_status(429));
    }

    #[tokio::test]
    async fn send_with_timeout_observes_pre_cancelled_token() {
        let token = CancellationToken::new();
        token.cancel();
        let request = reqwest::Client::new().get("http://127.0.0.1:9");

        let result = send_with_timeout(request, Some(&token), Duration::from_secs(1)).await;

        assert!(matches!(result, Err(TransportError::Cancelled)));
    }

    #[tokio::test]
    async fn read_body_rejects_oversized_body() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/body")
            .with_status(200)
            .with_body("abcdef")
            .create_async()
            .await;

        let response = reqwest::Client::new()
            .get(format!("{}/body", server.url()))
            .send()
            .await
            .expect("mock response");
        let result = read_body(response, None, Duration::from_secs(1), 3).await;

        assert!(matches!(
            result,
            Err(TransportError::BodyTooLarge { max_bytes: 3 })
        ));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn hardened_client_does_not_follow_redirects() {
        let mut server = mockito::Server::new_async().await;
        let redirect = server
            .mock("GET", "/start")
            .with_status(302)
            .with_header("location", "/elsewhere")
            .create_async()
            .await;
        let target = server
            .mock("GET", "/elsewhere")
            .with_status(200)
            .with_body("followed")
            .expect(0)
            .create_async()
            .await;

        let response = hardened_client()
            .get(format!("{}/start", server.url()))
            .send()
            .await
            .expect("redirect response surfaces without error");

        // The 3xx is surfaced verbatim; the Location target is never fetched.
        assert_eq!(response.status().as_u16(), 302);
        redirect.assert_async().await;
        target.assert_async().await;
    }
}
