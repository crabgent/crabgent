//! Configuration and error types for the `OpenAI` provider.

use std::fmt;
use std::time::Duration;

use crabgent_core::ProviderError;
use secrecy::SecretString;
use thiserror::Error;

/// Runtime configuration injected by the host application.
#[derive(Clone)]
pub struct OpenAiConfig {
    /// API key used by the standard `OpenAI` API path.
    pub api_key: SecretString,
    pub max_retries: u32,
    pub retry_base_delay: Duration,
    pub request_timeout: Duration,
}

impl OpenAiConfig {
    /// Build config with production defaults.
    pub fn new(api_key: impl Into<SecretString>) -> Self {
        Self {
            api_key: api_key.into(),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(500),
            request_timeout: Duration::from_secs(90),
        }
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

impl fmt::Debug for OpenAiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiConfig")
            .field("api_key", &"****<masked>")
            .field("max_retries", &self.max_retries)
            .field("retry_base_delay", &self.retry_base_delay)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

/// `OpenAI` provider-local error type.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OpenAiError {
    #[error("openai authentication failed")]
    Auth,
    #[error("openai network error: {0}")]
    Network(String),
    #[error("openai api error: status={status}")]
    Api {
        status: u16,
        retry_after_secs: Option<u64>,
    },
    #[error("openai malformed response: {0}")]
    MalformedResponse(String),
    #[error("openai config error: {0}")]
    ConfigError(String),
    /// `web_search` is not supported on the Chat Completions wire; use the
    /// Responses wire instead.
    #[error(
        "web_search is not supported on the Chat Completions wire; \
         use the Responses wire"
    )]
    WebSearchUnsupportedOnChatCompletions,
}

impl From<OpenAiError> for ProviderError {
    fn from(error: OpenAiError) -> Self {
        match error {
            OpenAiError::Auth => Self::Auth("openai authentication failed".to_owned()),
            OpenAiError::Network(message) => Self::Transport(message),
            OpenAiError::Api {
                status: 429,
                retry_after_secs,
            } => Self::RateLimited { retry_after_secs },
            OpenAiError::Api {
                status,
                retry_after_secs,
            } => Self::Api {
                status,
                message: "openai api request failed".to_owned(),
                retry_after_secs,
            },
            OpenAiError::MalformedResponse(message) => Self::MalformedResponse(message),
            OpenAiError::ConfigError(message) => Self::Other(message),
            OpenAiError::WebSearchUnsupportedOnChatCompletions => Self::Other(error.to_string()),
        }
    }
}
