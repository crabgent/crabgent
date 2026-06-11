//! Error surface for Slack adapter operations.

use std::time::Duration;

use thiserror::Error;

/// Errors emitted by Slack channel operations.
#[derive(Debug, Error)]
pub enum SlackError {
    /// Token was missing or had an invalid shape.
    #[error("invalid Slack token")]
    InvalidToken,
    /// Slack returned `ok=false`.
    #[error("Slack API error {slack_code} (http {http_status:?})")]
    ApiError {
        /// Slack error code.
        slack_code: String,
        /// HTTP status, when available.
        http_status: Option<u16>,
    },
    /// Transport failure.
    #[error("Slack transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// JSON serialization or parsing failure.
    #[error("Slack JSON error: {0}")]
    Serde(#[from] serde_json::Error),
    /// Authentication or authorization failed.
    #[error("Slack authentication failed")]
    Auth,
    /// Bot is not a member of the target conversation.
    #[error("Slack conversation membership required")]
    Membership,
    /// Slack rate-limited the call.
    #[error("Slack rate limited request")]
    RateLimited {
        /// Server-provided retry delay.
        retry_after: Option<Duration>,
    },
    /// Internal adapter failure.
    #[error("Slack internal error: {0}")]
    Internal(String),
}

impl SlackError {
    /// Map a Slack error code to the typed error surface.
    #[must_use]
    pub fn from_slack_code(code: String, http_status: Option<u16>) -> Self {
        match code.as_str() {
            "invalid_auth" | "token_revoked" | "not_authed" => Self::Auth,
            "not_in_channel" | "channel_not_found" | "no_permission" => Self::Membership,
            "ratelimited" => Self::RateLimited { retry_after: None },
            _ => Self::ApiError {
                slack_code: code,
                http_status,
            },
        }
    }
}
