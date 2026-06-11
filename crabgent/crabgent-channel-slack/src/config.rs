//! Runtime configuration for the Slack channel adapter.

use std::fmt::{self, Debug, Formatter};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;

/// Slack API configuration injected by the host application.
#[derive(Clone)]
pub struct SlackConfig {
    /// Socket Mode app token used for `apps.connections.open`.
    pub app_token: SecretString,
    /// Bot OAuth token used for Web API calls.
    pub bot_token: SecretString,
    /// Optional Slack Web API base URL. Defaults to `https://slack.com/api`.
    pub api_base: Option<String>,
    /// Per-request timeout.
    pub request_timeout: Duration,
    /// Maximum retries for retryable HTTP failures.
    pub retry_max: u32,
    /// Maximum outbound message body length in Unicode scalar values.
    pub body_cap_chars: usize,
}

impl SlackConfig {
    /// Build config with production defaults.
    pub fn new(app_token: SecretString, bot_token: SecretString) -> Result<Self, SlackConfigError> {
        let config = Self {
            app_token,
            bot_token,
            api_base: None,
            request_timeout: Duration::from_secs(30),
            retry_max: 2,
            body_cap_chars: 40_000,
        };
        config.validate()?;
        Ok(config)
    }

    /// Override the Slack API base URL, useful for tests.
    #[must_use]
    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = Some(api_base.into());
        self
    }

    /// Override the request timeout.
    #[must_use]
    pub const fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Override retry count.
    #[must_use]
    pub const fn with_retry_max(mut self, retry_max: u32) -> Self {
        self.retry_max = retry_max;
        self
    }

    /// Override the outbound body cap.
    #[must_use]
    pub const fn with_body_cap_chars(mut self, body_cap_chars: usize) -> Self {
        self.body_cap_chars = body_cap_chars;
        self
    }

    /// Return the configured API base or Slack's production endpoint.
    #[must_use]
    pub fn api_base(&self) -> &str {
        self.api_base.as_deref().unwrap_or("https://slack.com/api")
    }

    /// Validate required runtime inputs.
    pub fn validate(&self) -> Result<(), SlackConfigError> {
        if self.app_token.expose_secret().trim().is_empty() {
            return Err(SlackConfigError::Invalid("app_token must not be empty"));
        }
        if self.bot_token.expose_secret().trim().is_empty() {
            return Err(SlackConfigError::Invalid("bot_token must not be empty"));
        }
        if self.request_timeout.is_zero() {
            return Err(SlackConfigError::Invalid("request_timeout must be > 0"));
        }
        if self.api_base().trim().is_empty() {
            return Err(SlackConfigError::Invalid("api_base must not be empty"));
        }
        if self.body_cap_chars == 0 {
            return Err(SlackConfigError::Invalid("body_cap_chars must be > 0"));
        }
        Ok(())
    }
}

impl Debug for SlackConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlackConfig")
            .field("app_token", &"[REDACTED]")
            .field("bot_token", &"[REDACTED]")
            .field("api_base", &self.api_base)
            .field("request_timeout", &self.request_timeout)
            .field("retry_max", &self.retry_max)
            .field("body_cap_chars", &self.body_cap_chars)
            .finish()
    }
}

/// Invalid Slack configuration.
#[derive(Error, PartialEq, Eq)]
pub enum SlackConfigError {
    /// A field failed validation.
    #[error("invalid Slack config: {0}")]
    Invalid(&'static str),
}

impl Debug for SlackConfigError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => f.debug_tuple("Invalid").field(message).finish(),
        }
    }
}

impl From<SlackConfigError> for crate::SlackError {
    fn from(value: SlackConfigError) -> Self {
        Self::Internal(value.to_string())
    }
}
