//! Error surface for the Matrix channel adapter.

use thiserror::Error;

/// Errors emitted by Matrix channel operations.
#[derive(Debug, Error)]
pub enum MatrixChannelError {
    /// Login or auth flow failed.
    #[error("login failed: {0}")]
    Login(String),

    /// Message send failed.
    #[error("send failed: {0}")]
    Send(String),

    /// Sync loop failed.
    #[error("sync failed: {0}")]
    Sync(String),

    /// Configuration was invalid.
    #[error("invalid config: {0}")]
    Config(String),

    /// Network failure.
    #[error("network failure: {0}")]
    Network(String),
}

impl From<MatrixChannelError> for crabgent_channel::ChannelError {
    fn from(value: MatrixChannelError) -> Self {
        match value {
            MatrixChannelError::Config(msg) => Self::InvalidOwnerFormat(msg),
            other => Self::adapter(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_channel::ChannelError;

    #[test]
    fn config_error_maps_to_invalid_owner_format() {
        let err = ChannelError::from(MatrixChannelError::Config("bad owner".into()));
        assert!(matches!(err, ChannelError::InvalidOwnerFormat(_)));
    }

    #[test]
    fn send_error_maps_to_adapter_error() {
        let err = ChannelError::from(MatrixChannelError::Send("m.server".into()));
        assert!(matches!(err, ChannelError::Adapter(_)));
    }
}
