//! Configuration and error types for the Google provider.

use std::fmt;
use std::time::Duration;

use secrecy::SecretString;
use thiserror::Error;

/// Runtime configuration injected by the host application.
#[derive(Clone)]
pub struct GoogleConfig {
    pub api_key: SecretString,
    pub base_url: String,
    pub api_version: String,
    pub max_retries: u32,
    pub retry_base_delay: Duration,
    pub request_timeout: Duration,
}

impl GoogleConfig {
    /// Build config with Gemini API defaults.
    pub fn new(api_key: impl Into<SecretString>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://generativelanguage.googleapis.com".to_owned(),
            api_version: "v1beta".to_owned(),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(500),
            request_timeout: Duration::from_secs(90),
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    #[must_use]
    pub fn with_api_version(mut self, api_version: impl Into<String>) -> Self {
        self.api_version = api_version.into();
        self
    }

    #[must_use]
    pub const fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    #[must_use]
    pub const fn with_retry_base_delay(mut self, retry_base_delay: Duration) -> Self {
        self.retry_base_delay = retry_base_delay;
        self
    }

    #[must_use]
    pub const fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }
}

impl fmt::Debug for GoogleConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleConfig")
            .field("api_key", &"****<masked>")
            .field("base_url", &self.base_url)
            .field("api_version", &self.api_version)
            .field("max_retries", &self.max_retries)
            .field("retry_base_delay", &self.retry_base_delay)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

/// Google provider-local error type.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GoogleError {
    #[error("google authentication failed")]
    Auth,
    #[error("google network error")]
    Network,
    #[error("google api error: status={status}")]
    Api {
        status: u16,
        retry_after_secs: Option<u64>,
    },
    #[error("google malformed response: {0}")]
    MalformedResponse(String),
    #[error("google config error: {0}")]
    ConfigError(String),
    #[error("cancelled")]
    Cancelled,
    #[error("timeout")]
    Timeout,
}
