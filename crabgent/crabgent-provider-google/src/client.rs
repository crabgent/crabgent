//! Shared HTTP helpers for the Google Gemini API.

use std::time::Duration;

use bytes::Bytes;
use crabgent_log::warn;
use crabgent_provider_transport::{
    DEFAULT_MAX_BACKOFF, ErrorBodyMode, HttpStatusError, RetryLifecycleConfig,
    RetryLifecycleOutcome, TransportError, longer_retry_delay, read_body as read_capped_body,
    send_with_retry_lifecycle,
};
use reqwest::header::{CONTENT_TYPE, HeaderName};
use secrecy::ExposeSecret;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::types::{GoogleConfig, GoogleError};

const API_KEY_HEADER: HeaderName = HeaderName::from_static("x-goog-api-key");
pub(crate) const MAX_JSON_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

pub(crate) async fn post_json(
    http: &reqwest::Client,
    config: &GoogleConfig,
    model: &str,
    body: &Value,
    max_response_bytes: usize,
    cancel: Option<&CancellationToken>,
) -> Result<Value, GoogleError> {
    let endpoint = generate_content_url(config, model);
    let response = send_with_retry(http, config, &endpoint, body, cancel).await?;
    let bytes = read_body(response, cancel, config.request_timeout, max_response_bytes).await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| GoogleError::MalformedResponse(error.to_string()))
}

pub(crate) async fn post_stream(
    http: &reqwest::Client,
    config: &GoogleConfig,
    model: &str,
    body: &Value,
    cancel: Option<&CancellationToken>,
) -> Result<reqwest::Response, GoogleError> {
    let endpoint = stream_generate_content_url(config, model);
    send_with_retry(http, config, &endpoint, body, cancel).await
}

async fn get_json_at(
    http: &reqwest::Client,
    config: &GoogleConfig,
    endpoint: &str,
) -> Result<Value, GoogleError> {
    let request = http
        .get(endpoint)
        .header(API_KEY_HEADER, config.api_key.expose_secret());
    let response = tokio::time::timeout(config.request_timeout, request.send())
        .await
        .map_err(|_elapsed| GoogleError::Timeout)?
        .map_err(|_error| GoogleError::Network)?;
    if !response.status().is_success() {
        return Err(map_http_status(response.status().as_u16()));
    }
    let bytes = read_body(
        response,
        None,
        config.request_timeout,
        MAX_JSON_RESPONSE_BYTES,
    )
    .await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| GoogleError::MalformedResponse(error.to_string()))
}

pub(crate) async fn get_json(
    http: &reqwest::Client,
    config: &GoogleConfig,
    path: &str,
) -> Result<Value, GoogleError> {
    get_json_at(http, config, &endpoint_url(config, path)).await
}

pub async fn create_cached_content(
    http: &reqwest::Client,
    config: &GoogleConfig,
    body: &Value,
    cancel: Option<&CancellationToken>,
) -> Result<Value, GoogleError> {
    let endpoint = endpoint_url(config, "/cachedContents");
    let response = send_with_retry(http, config, &endpoint, body, cancel).await?;
    let bytes = read_body(
        response,
        cancel,
        config.request_timeout,
        MAX_JSON_RESPONSE_BYTES,
    )
    .await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| GoogleError::MalformedResponse(error.to_string()))
}

pub async fn get_cached_content(
    http: &reqwest::Client,
    config: &GoogleConfig,
    name: &str,
) -> Result<Value, GoogleError> {
    get_json_at(http, config, &endpoint_url(config, name)).await
}

async fn send_with_retry(
    http: &reqwest::Client,
    config: &GoogleConfig,
    endpoint: &str,
    body: &Value,
    cancel: Option<&CancellationToken>,
) -> Result<reqwest::Response, GoogleError> {
    let noop = CancellationToken::new();
    let token = cancel.unwrap_or(&noop);
    let outcome = send_with_retry_lifecycle(
        || {
            http.post(endpoint)
                .header(CONTENT_TYPE, "application/json")
                .header(API_KEY_HEADER, config.api_key.expose_secret())
                .json(body)
        },
        token,
        RetryLifecycleConfig {
            max_retries: config.max_retries,
            request_timeout: config.request_timeout,
            error_body_max_bytes: MAX_ERROR_BODY_BYTES,
            error_body_mode: ErrorBodyMode::Drain,
        },
        is_retryable_status,
        |attempt, retry_after| retry_delay(attempt, config.retry_base_delay, retry_after),
    )
    .await
    .map_err(|error| map_transport_error(&error))?;
    map_retry_outcome(outcome)
}

fn map_retry_outcome(outcome: RetryLifecycleOutcome) -> Result<reqwest::Response, GoogleError> {
    match outcome {
        RetryLifecycleOutcome::Ok(response) => Ok(response),
        RetryLifecycleOutcome::HttpError(error) => Err(map_http_error(&error)),
        RetryLifecycleOutcome::Network(_error) => Err(GoogleError::Network),
        RetryLifecycleOutcome::Timeout => Err(GoogleError::Timeout),
    }
}

fn map_http_error(error: &HttpStatusError) -> GoogleError {
    if is_auth_status(error.status) {
        return map_auth_error(error.status);
    }
    map_api_error(error)
}

fn map_auth_error(status: u16) -> GoogleError {
    warn!(status = status, "google authentication failed");
    GoogleError::Auth
}

fn map_api_error(error: &HttpStatusError) -> GoogleError {
    warn!(status = error.status, "google api request failed");
    GoogleError::Api {
        status: error.status,
        retry_after_secs: error.retry_after.map(|delay| delay.as_secs()),
    }
}

pub(crate) async fn read_body(
    response: reqwest::Response,
    cancel: Option<&CancellationToken>,
    timeout: Duration,
    max_bytes: usize,
) -> Result<Bytes, GoogleError> {
    read_capped_body(response, cancel, timeout, max_bytes)
        .await
        .map_err(|error| map_transport_error(&error))
}

pub(crate) fn generate_content_url(config: &GoogleConfig, model: &str) -> String {
    endpoint_url(
        config,
        &format!(
            "/models/{}:generateContent",
            model.trim_start_matches("models/")
        ),
    )
}

pub(crate) fn stream_generate_content_url(config: &GoogleConfig, model: &str) -> String {
    format!(
        "{}?alt=sse",
        endpoint_url(
            config,
            &format!(
                "/models/{}:streamGenerateContent",
                model.trim_start_matches("models/")
            ),
        )
    )
}

fn endpoint_url(config: &GoogleConfig, path: &str) -> String {
    format!(
        "{}/{}/{}",
        config.base_url.trim_end_matches('/'),
        config.api_version.trim_matches('/'),
        path.trim_start_matches('/')
    )
}

fn retry_delay(attempt: u32, base: Duration, retry_after: Option<Duration>) -> Duration {
    longer_retry_delay(attempt, base, retry_after, DEFAULT_MAX_BACKOFF)
}

const fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500..=599)
}

const fn is_auth_status(status: u16) -> bool {
    matches!(status, 401 | 403)
}

const fn map_http_status(status: u16) -> GoogleError {
    if is_auth_status(status) {
        GoogleError::Auth
    } else {
        GoogleError::Api {
            status,
            retry_after_secs: None,
        }
    }
}

fn map_transport_error(error: &TransportError) -> GoogleError {
    match error {
        TransportError::Cancelled => GoogleError::Cancelled,
        TransportError::Timeout => GoogleError::Timeout,
        TransportError::BodyTooLarge { .. } => {
            GoogleError::MalformedResponse("google response exceeds max body size".to_owned())
        }
        _ => GoogleError::Network,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_respects_long_retry_after() {
        let delay = retry_delay(0, Duration::from_secs(1), Some(Duration::from_secs(90)));

        assert_eq!(delay, Duration::from_secs(90));
    }

    #[test]
    fn retry_delay_keeps_larger_local_backoff() {
        let delay = retry_delay(5, Duration::from_secs(1), Some(Duration::from_secs(2)));

        assert_eq!(delay, DEFAULT_MAX_BACKOFF);
    }
}
