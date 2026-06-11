//! HTTP transport for the Anthropic Messages API. Handles auth headers,
//! retry on transient failures (connect errors + 429/500/529), and
//! cancellation propagation.

use std::time::Duration;

use crabgent_core::{LlmRequest, LlmResponse, ProviderError, text::truncate_with_ellipsis};
use crabgent_log::warn;
use crabgent_provider_transport::{
    ErrorBodyMode, HttpStatusError, RetryLifecycleConfig, RetryLifecycleOutcome, TransportError,
    read_body as read_capped_body, send_with_retry_lifecycle,
};
use reqwest::header::HeaderMap;
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::request::{RequestBuildError, build_body};
use crate::response::parse as parse_complete_body;
use crate::retry::{is_retryable_status, parse_retry_after, retry_sleep_delay};
use crate::types::{AnthropicConfig, api_key_is_header_safe};

const ANTHROPIC_VERSION_HEADER: &str = "anthropic-version";
const ANTHROPIC_BETA_HEADER: &str = "anthropic-beta";
const API_KEY_HEADER: &str = "x-api-key";
const MAX_RESPONSE_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_ERROR_BODY_SNIPPET_BYTES: usize = 2_048;
const API_ERROR_MESSAGE: &str = "anthropic api request failed";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConfigError {
    #[error("anthropic api_key must not be empty or whitespace-only")]
    EmptyKey,
    #[error("anthropic api_key contains illegal HTTP header characters")]
    InvalidFormat,
    #[error("anthropic cache_ttl must be one of \"5m\" or \"1h\", got: {0}")]
    InvalidCacheTtl(String),
}

/// Thin wrapper around `reqwest::Client` that knows how to talk to the
/// Anthropic Messages API. Used by the higher-level Provider impl.
#[derive(Clone, Debug)]
pub struct AnthropicClient {
    http: reqwest::Client,
    config: AnthropicConfig,
}

impl AnthropicClient {
    /// Build a new client and return a typed configuration error
    /// instead of panicking.
    pub fn try_new(http: reqwest::Client, config: AnthropicConfig) -> Result<Self, ConfigError> {
        if config.api_key.trim().is_empty() {
            return Err(ConfigError::EmptyKey);
        }
        if !api_key_is_header_safe(&config.api_key) {
            return Err(ConfigError::InvalidFormat);
        }
        // Defense-in-depth for direct field assignment; with_cache_ttl validates first.
        if let Some(ttl) = config.cache_ttl()
            && ttl != "5m"
            && ttl != "1h"
        {
            return Err(ConfigError::InvalidCacheTtl(ttl.to_string()));
        }
        Ok(Self { http, config })
    }

    #[must_use]
    pub const fn config(&self) -> &AnthropicConfig {
        &self.config
    }

    #[must_use]
    pub(crate) const fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Open a streaming Messages API request. Returns the underlying
    /// `reqwest::Response`; the caller consumes its `bytes_stream()` and
    /// feeds bytes into the SSE parser.
    pub async fn start_stream(
        &self,
        req: &LlmRequest,
        cancel: Option<&CancellationToken>,
    ) -> Result<reqwest::Response, ProviderError> {
        let body = build_body(req, true, self.config.cache_ttl(), req.model.as_str())
            .map_err(map_build_error)?;
        self.send_with_retry(&body, cancel).await
    }

    /// Fire a non-streaming Messages API request and return the parsed
    /// `LlmResponse`.
    pub async fn call_complete(
        &self,
        req: &LlmRequest,
        cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let body = build_body(req, false, self.config.cache_ttl(), req.model.as_str())
            .map_err(map_build_error)?;
        let response = self.send_with_retry(&body, cancel).await?;
        let bytes = read_body(
            response,
            cancel,
            self.config.complete_timeout,
            MAX_RESPONSE_BODY_BYTES,
        )
        .await?;
        let parsed: Value = serde_json::from_slice(&bytes)
            .map_err(|e| ProviderError::MalformedResponse(e.to_string()))?;
        parse_complete_body(&parsed)
    }

    fn build_http_request(&self, body: &Value) -> reqwest::RequestBuilder {
        let url = format!("{}/v1/messages", self.config.endpoint);
        let mut builder = self
            .http
            .post(url)
            .header("content-type", "application/json")
            .header(ANTHROPIC_VERSION_HEADER, &self.config.anthropic_version)
            .header(API_KEY_HEADER, &self.config.api_key);
        if let Some(beta) = build_beta_header(self.config.anthropic_betas()) {
            builder = builder.header(ANTHROPIC_BETA_HEADER, beta);
        }
        builder.json(body)
    }

    // The retry lifecycle owns the per-attempt timeout
    // (`request_timeout: complete_timeout`). Wrapping the whole retry loop in
    // an outer `tokio::time::timeout(complete_timeout, ...)` would give the
    // loop only a single attempt's budget and fire a spurious `Timeout` while
    // later retries are still in flight, so there is no outer guard here. This
    // mirrors the OpenAI sibling's `send_with_retry`.
    async fn send_with_retry(
        &self,
        body: &Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<reqwest::Response, ProviderError> {
        let noop = CancellationToken::new();
        let token = cancel.unwrap_or(&noop);
        let outcome = send_with_retry_lifecycle(
            || self.build_http_request(body),
            token,
            RetryLifecycleConfig {
                max_retries: self.config.max_retries,
                request_timeout: self.config.complete_timeout,
                error_body_max_bytes: MAX_ERROR_BODY_BYTES,
                error_body_mode: ErrorBodyMode::Text,
            },
            is_retryable_status,
            |attempt, retry_after| {
                retry_sleep_delay(attempt, self.config.retry_base_delay, retry_after)
            },
        )
        .await
        .map_err(map_transport_error)?;
        match outcome {
            RetryLifecycleOutcome::Ok(response) => Ok(response),
            RetryLifecycleOutcome::HttpError(error) => {
                Err(map_http_error(&error, &self.config.api_key))
            }
            RetryLifecycleOutcome::Network(error) => {
                Err(ProviderError::Transport(error.to_string()))
            }
            RetryLifecycleOutcome::Timeout => Err(ProviderError::Timeout),
        }
    }
}

async fn read_body(
    resp: reqwest::Response,
    cancel: Option<&CancellationToken>,
    timeout: Duration,
    max_bytes: usize,
) -> Result<bytes::Bytes, ProviderError> {
    read_capped_body(resp, cancel, timeout, max_bytes)
        .await
        .map_err(map_transport_error)
}

fn map_transport_error(error: TransportError) -> ProviderError {
    match error {
        TransportError::Cancelled => ProviderError::Cancelled,
        TransportError::Timeout => ProviderError::Timeout,
        TransportError::Request(error) => map_reqwest_error(&error),
        TransportError::BodyTooLarge { max_bytes } => ProviderError::MalformedResponse(format!(
            "anthropic response exceeds max body size: {max_bytes} bytes"
        )),
        _ => ProviderError::Transport("anthropic transport failed".to_owned()),
    }
}

fn map_reqwest_error(err: &reqwest::Error) -> ProviderError {
    if err.is_timeout() {
        ProviderError::Timeout
    } else {
        ProviderError::Transport(err.to_string())
    }
}

fn map_build_error(e: RequestBuildError) -> ProviderError {
    ProviderError::Other(e.to_string())
}

fn map_http_error(error: &HttpStatusError, api_key: &str) -> ProviderError {
    let status = error.status;
    let retry_after_secs = error.retry_after.map(|d| d.as_secs());
    let body_len = error.body.as_deref().map_or(0, str::len);
    match status {
        401 | 403 => map_auth_status(status, body_len),
        429 => map_rate_limit_status(status, body_len, retry_after_secs),
        _ => map_api_status(
            status,
            body_len,
            retry_after_secs,
            error.body.as_deref(),
            api_key,
        ),
    }
}

fn map_auth_status(status: u16, body_len: usize) -> ProviderError {
    warn!(status, body_len, "anthropic auth failure");
    ProviderError::Auth("anthropic authentication failed".into())
}

fn map_rate_limit_status(
    status: u16,
    body_len: usize,
    retry_after_secs: Option<u64>,
) -> ProviderError {
    warn!(status, body_len, "anthropic rate limited");
    ProviderError::RateLimited { retry_after_secs }
}

fn map_api_status(
    status: u16,
    body_len: usize,
    retry_after_secs: Option<u64>,
    body: Option<&str>,
    api_key: &str,
) -> ProviderError {
    warn!(status, body_len, "anthropic api request failed");
    ProviderError::Api {
        status,
        message: api_error_message(body, api_key),
        retry_after_secs,
    }
}

fn api_error_message(body: Option<&str>, api_key: &str) -> String {
    let Some(body) = body.map(str::trim).filter(|body| !body.is_empty()) else {
        return API_ERROR_MESSAGE.to_owned();
    };
    let redacted = crate::redact::redact_error_body(body, api_key);
    let snippet = truncate_with_ellipsis(redacted.trim(), MAX_ERROR_BODY_SNIPPET_BYTES, "...");
    if snippet.is_empty() {
        API_ERROR_MESSAGE.to_owned()
    } else {
        format!("{API_ERROR_MESSAGE}: {snippet}")
    }
}

fn build_beta_header(betas: &[String]) -> Option<String> {
    let v: Vec<&str> = betas
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if v.is_empty() {
        None
    } else {
        Some(v.join(","))
    }
}

/// Read a `HeaderMap`'s `retry-after` and return seconds. Re-exported
/// for tests that want to verify the header parsing in isolation.
#[must_use]
#[doc(hidden)]
pub fn _retry_after_secs(headers: &HeaderMap) -> Option<u64> {
    parse_retry_after(headers).map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::ToolDef;
    use serde_json::json;

    fn cfg(api_key: &str) -> AnthropicConfig {
        AnthropicConfig::new(api_key)
            .with_max_retries(0)
            .with_retry_base_delay(Duration::from_millis(1))
    }

    fn cfg_with_ttl(ttl: Option<&str>) -> AnthropicConfig {
        cfg("sk-ant-api03-x")
            .with_cache_ttl(ttl.map(str::to_string))
            .expect("valid ttl")
    }

    fn req() -> LlmRequest {
        LlmRequest {
            model: "claude-haiku-4-5".into(),
            system_prompt: None,
            messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})],
            tools: vec![ToolDef {
                name: "noop".into(),
                description: "stub".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: Some(64),
            temperature: None,
            stop_sequences: vec![],
            reasoning_effort: None,
            web_search: crabgent_core::types::WebSearchConfig::default(),
            tool_choice: None,
        }
    }

    fn build_test_body(req: &LlmRequest, stream: bool, cache_ttl: Option<&str>) -> Value {
        build_body(req, stream, cache_ttl, req.model.as_str()).expect("test build_body")
    }

    fn http_error(status: u16, body: Option<&str>) -> HttpStatusError {
        HttpStatusError {
            status,
            retry_after: None,
            body: body.map(str::to_owned),
        }
    }

    #[test]
    fn beta_header_joins_or_returns_none() {
        assert_eq!(build_beta_header(&[]), None);
        assert_eq!(
            build_beta_header(&["a".into(), "b".into()]),
            Some("a,b".into())
        );
        assert_eq!(
            build_beta_header(&["  ".into(), "x".into(), String::new()]),
            Some("x".into())
        );
    }

    #[test]
    fn map_status_classifies_known_codes() {
        let e = map_http_error(&http_error(401, None), "sk-ant-api03-x");
        assert!(matches!(e, ProviderError::Auth(s) if s == "anthropic authentication failed"));
        let e = map_http_error(&http_error(403, None), "sk-ant-api03-x");
        assert!(matches!(e, ProviderError::Auth(_)));
        let e = map_http_error(
            &HttpStatusError {
                status: 429,
                retry_after: Some(Duration::from_secs(7)),
                body: Some("rate limit".into()),
            },
            "sk-ant-api03-x",
        );
        match e {
            ProviderError::RateLimited { retry_after_secs } => {
                assert_eq!(retry_after_secs, Some(7));
            }
            other => panic!("unexpected: {other:?}"),
        }
        let e = map_http_error(&http_error(400, Some("max_tokens: 32768 > 32000")), "sk");
        assert!(matches!(e, ProviderError::Api { status: 400, .. }));
        assert!(e.to_string().contains("max_tokens"));
        let e = map_http_error(&http_error(500, None), "sk-ant-api03-x");
        assert!(matches!(e, ProviderError::Api { status: 500, .. }));
    }

    #[test]
    fn try_new_rejects_empty_key() {
        let err =
            AnthropicClient::try_new(reqwest::Client::new(), cfg("")).expect_err("expected error");
        assert!(matches!(err, ConfigError::EmptyKey));
    }

    #[test]
    fn try_new_rejects_whitespace_only_key() {
        let err = AnthropicClient::try_new(reqwest::Client::new(), cfg("   "))
            .expect_err("expected error");
        assert!(matches!(err, ConfigError::EmptyKey));
    }

    #[test]
    fn try_new_accepts_valid_key() {
        let client = AnthropicClient::try_new(reqwest::Client::new(), cfg("sk-ant-api03-x"))
            .expect("valid key");
        assert_eq!(client.config().api_key, "sk-ant-api03-x");
    }

    #[test]
    fn try_new_accepts_valid_cache_ttls_and_none() {
        for ttl in [Some("5m"), Some("1h"), None] {
            let client = AnthropicClient::try_new(reqwest::Client::new(), cfg_with_ttl(ttl));
            client.expect("test result");
        }
    }

    #[test]
    fn build_http_request_attaches_headers_and_body() {
        let client = AnthropicClient::try_new(
            reqwest::Client::new(),
            cfg("sk-ant-api03-x").with_betas(vec!["beta-x".into()]),
        )
        .expect("valid config");
        let body = build_test_body(&req(), false, None);
        let r = client.build_http_request(&body).build().expect("build");
        let headers = r.headers();
        assert_eq!(
            headers
                .get(ANTHROPIC_VERSION_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some(AnthropicConfig::DEFAULT_VERSION),
        );
        assert_eq!(
            headers.get(API_KEY_HEADER).and_then(|v| v.to_str().ok()),
            Some("sk-ant-api03-x"),
        );
        assert_eq!(
            headers
                .get(ANTHROPIC_BETA_HEADER)
                .and_then(|v| v.to_str().ok()),
            // Merge-semantics: EXTENDED_CACHE_TTL_BETA was injected by
            // `new()`, then `with_betas(vec!["beta-x"])` appends "beta-x"
            // behind it (dedup preserves insertion order).
            Some("extended-cache-ttl-2025-04-11,beta-x"),
        );
    }

    #[test]
    fn build_http_request_retains_cache_beta_after_cache_disabled() {
        // Merge-semantics on `with_betas` removes the only public clear
        // path: `EXTENDED_CACHE_TTL_BETA` injected by `new()` survives
        // `with_cache_ttl(None)` and an empty `with_betas`. The header
        // therefore stays present and reflects the persisted beta entry.
        let config = cfg("k")
            .with_cache_ttl(None)
            .expect("valid ttl")
            .with_betas(Vec::new());
        let client =
            AnthropicClient::try_new(reqwest::Client::new(), config).expect("valid config");
        let body = build_test_body(&req(), false, None);
        let r = client.build_http_request(&body).build().expect("build");
        assert_eq!(
            r.headers()
                .get(ANTHROPIC_BETA_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some("extended-cache-ttl-2025-04-11"),
        );
    }

    #[tokio::test]
    async fn send_with_retry_returns_cancelled_when_token_set_before_call() {
        let client =
            AnthropicClient::try_new(reqwest::Client::new(), cfg("k")).expect("valid config");
        let body = build_test_body(&req(), false, None);
        let token = CancellationToken::new();
        token.cancel();
        let r = client.send_with_retry(&body, Some(&token)).await;
        assert!(matches!(r, Err(ProviderError::Cancelled)));
    }
}
