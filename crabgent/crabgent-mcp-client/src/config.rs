use std::fmt;
use std::time::Duration;

use secrecy::SecretString;

use crate::McpError;

pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 1_048_576;
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 5_242_880;

/// Default per-read idle deadline for the HTTP transport. Resets after every
/// successful read, so a healthy long-lived SSE stream survives while a
/// slow-loris server that trickles bytes below the response cap trips it.
pub const DEFAULT_READ_IDLE_TIMEOUT: Duration = Duration::from_mins(1);

#[derive(Clone)]
pub struct McpServerConfig {
    // Initial: bare String, Newtype pending
    pub name: String,
    pub base_url: String,
    pub token: Option<SecretString>,
    pub max_response_bytes: usize,
    pub max_output_bytes: usize,
    pub read_idle_timeout: Duration,
}

impl McpServerConfig {
    pub fn new(name: impl Into<String>, base_url: impl Into<String>) -> Result<Self, McpError> {
        let name = name.into();
        validate_server_name(&name)?;

        Ok(Self {
            name,
            base_url: base_url.into(),
            token: None,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            read_idle_timeout: DEFAULT_READ_IDLE_TIMEOUT,
        })
    }

    pub fn with_token(mut self, token: SecretString) -> Self {
        self.token = Some(token);
        self
    }

    pub const fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    pub const fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }

    pub const fn with_read_idle_timeout(mut self, read_idle_timeout: Duration) -> Self {
        self.read_idle_timeout = read_idle_timeout;
        self
    }
}

impl fmt::Debug for McpServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServerConfig")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("token", &self.token.as_ref().map(|_| "Bearer ****"))
            .field("max_response_bytes", &self.max_response_bytes)
            .field("max_output_bytes", &self.max_output_bytes)
            .field("read_idle_timeout", &self.read_idle_timeout)
            .finish()
    }
}

pub(crate) fn validate_server_name(name: &str) -> Result<(), McpError> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');

    if valid {
        Ok(())
    } else {
        Err(McpError::InvalidConfig(
            "server name must match ^[a-zA-Z0-9_-]+$".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;

    use super::{DEFAULT_MAX_OUTPUT_BYTES, McpServerConfig};

    #[test]
    fn name_valid_chars_pass() {
        let config = McpServerConfig::new("server_42-prod", "http://localhost:3000/mcp");

        config.expect("test result");
    }

    #[test]
    fn name_invalid_chars_returns_error() {
        for name in ["", "server name", "server.name", "server/name"] {
            let err = McpServerConfig::new(name, "http://localhost:3000/mcp")
                .expect_err("invalid server name should fail");

            assert!(err.to_string().contains("server name must match"));
        }
    }

    #[test]
    fn token_redacted_in_debug() {
        let config = McpServerConfig::new("server_42", "http://localhost:3000/mcp")
            .expect("valid test config")
            .with_token(SecretString::from("secret-test-token-12345".to_string()));

        let debug = format!("{config:?}");

        assert!(debug.contains("Bearer ****"));
        assert!(!debug.contains("secret-test-token-12345"));
    }

    #[test]
    fn mcp_relax_default_max_output_bytes() {
        assert_eq!(DEFAULT_MAX_OUTPUT_BYTES, 5_242_880);
    }
}
