//! HTTP request lifecycle for image generation: retry, auth-refresh retry,
//! body reading, and error mapping into [`ImageGenerationError`].

use crabgent_core::{ImageGenerationError, RunCtx};
use crabgent_log::warn;
use crabgent_provider_transport::{
    ErrorBodyMode, HttpStatusError, RetryLifecycleConfig, RetryLifecycleOutcome, TransportError,
    is_auth_status, join_url_path, read_body as read_capped_body, send_with_retry_lifecycle,
};
use reqwest::header::CONTENT_TYPE;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::auth::AuthStrategy;
use crate::retry::{is_retryable_status, sleep_delay};
use crate::types::OpenAiConfig;

use super::decode_error;

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Borrowed request context shared by the send and auth-refresh-retry paths.
///
/// Bundling these references keeps both entry points within the argument
/// budget while preserving the exact control flow they shared before.
pub(super) struct ImageRequestCtx<'a> {
    pub http: &'a reqwest::Client,
    pub config: &'a OpenAiConfig,
    pub auth: &'a dyn AuthStrategy,
    pub ctx: &'a RunCtx,
    pub body: &'a Value,
    pub endpoint_path: &'a str,
}

pub(super) async fn send_with_retry(
    request: &ImageRequestCtx<'_>,
    cancel: Option<&CancellationToken>,
) -> Result<reqwest::Response, ImageGenerationError> {
    let outcome = run_retry_lifecycle(request, cancel).await?;
    match outcome {
        RetryLifecycleOutcome::HttpError(error) if is_auth_status(error.status) => {
            retry_after_auth_refresh(request, cancel, error.status).await
        }
        other => map_retry_outcome(other),
    }
}

async fn retry_after_auth_refresh(
    request: &ImageRequestCtx<'_>,
    cancel: Option<&CancellationToken>,
    status: u16,
) -> Result<reqwest::Response, ImageGenerationError> {
    if !request
        .auth
        .refresh_after_auth_error()
        .await
        .map_err(|error| map_provider_auth_refresh_error(&error))?
    {
        return Err(map_auth_error(status));
    }
    warn!(
        status = status,
        "openai image generation authentication refreshed after auth failure; retrying request"
    );
    let outcome = run_retry_lifecycle(request, cancel).await?;
    map_retry_outcome(outcome)
}

async fn run_retry_lifecycle(
    request: &ImageRequestCtx<'_>,
    cancel: Option<&CancellationToken>,
) -> Result<RetryLifecycleOutcome, ImageGenerationError> {
    let noop = CancellationToken::new();
    let token = cancel.unwrap_or(&noop);
    send_with_retry_lifecycle(
        || build_request(request),
        token,
        RetryLifecycleConfig {
            max_retries: request.config.max_retries,
            request_timeout: request.config.request_timeout,
            error_body_max_bytes: MAX_ERROR_BODY_BYTES,
            error_body_mode: ErrorBodyMode::Drain,
        },
        is_retryable_status,
        |attempt, retry_after| sleep_delay(attempt, request.config.retry_base_delay, retry_after),
    )
    .await
    .map_err(|error| map_transport_error(&error))
}

fn map_retry_outcome(
    outcome: RetryLifecycleOutcome,
) -> Result<reqwest::Response, ImageGenerationError> {
    match outcome {
        RetryLifecycleOutcome::Ok(response) => Ok(response),
        RetryLifecycleOutcome::HttpError(error) => Err(map_http_error(&error)),
        RetryLifecycleOutcome::Network(_error) => Err(ImageGenerationError::Network),
        RetryLifecycleOutcome::Timeout => Err(ImageGenerationError::Timeout),
    }
}

fn map_http_error(error: &HttpStatusError) -> ImageGenerationError {
    if is_auth_status(error.status) {
        return map_auth_error(error.status);
    }
    map_backend_error(error.status)
}

fn map_auth_error(status: u16) -> ImageGenerationError {
    warn!(
        status = status,
        "openai image generation authentication failed"
    );
    ImageGenerationError::Auth("openai authentication failed".to_owned())
}

fn map_provider_auth_refresh_error(error: &crabgent_core::ProviderError) -> ImageGenerationError {
    warn!(
        error = %error,
        "openai image generation authentication refresh failed"
    );
    ImageGenerationError::Auth("openai authentication failed".to_owned())
}

fn map_backend_error(status: u16) -> ImageGenerationError {
    warn!(status = status, "openai image generation request failed");
    ImageGenerationError::Backend("openai image generation request failed".to_owned())
}

fn build_request(request: &ImageRequestCtx<'_>) -> reqwest::RequestBuilder {
    let url = endpoint_url(request.auth.base_url(), request.endpoint_path);
    let mut builder = request
        .http
        .post(url)
        .header(CONTENT_TYPE, "application/json");
    for (name, value) in request.auth.auth_headers() {
        builder = builder.header(name, value);
    }
    for (name, value) in request.auth.request_headers(request.ctx) {
        builder = builder.header(name, value);
    }
    builder.json(request.body)
}

fn endpoint_url(base_url: &str, endpoint_path: &str) -> String {
    join_url_path(base_url, endpoint_path)
}

pub(super) async fn read_body(
    response: reqwest::Response,
    cancel: Option<&CancellationToken>,
    timeout: std::time::Duration,
    max_bytes: usize,
) -> Result<bytes::Bytes, ImageGenerationError> {
    read_capped_body(response, cancel, timeout, max_bytes)
        .await
        .map_err(|error| map_transport_error(&error))
}

fn map_transport_error(error: &TransportError) -> ImageGenerationError {
    match error {
        TransportError::Cancelled => ImageGenerationError::Cancelled,
        TransportError::Timeout => ImageGenerationError::Timeout,
        TransportError::BodyTooLarge { .. } => decode_error("image response exceeds max body size"),
        _ => ImageGenerationError::Network,
    }
}
