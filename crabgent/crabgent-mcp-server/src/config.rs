use crabgent_core::{ModelTarget, Subject};
use secrecy::SecretString;

pub const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
pub const DEFAULT_MAX_SESSIONS: usize = 1_024;
pub const DEFAULT_MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct McpServerConfig {
    pub bearer_token: SecretString,
    pub default_model: ModelTarget,
    pub protocol_version: &'static str,
    pub max_sessions: usize,
    pub max_request_bytes: usize,
    pub(crate) subject_override: Option<Subject>,
}

impl McpServerConfig {
    #[must_use]
    pub const fn new(bearer_token: SecretString, default_model: ModelTarget) -> Self {
        Self {
            bearer_token,
            default_model,
            protocol_version: MCP_PROTOCOL_VERSION,
            max_sessions: DEFAULT_MAX_SESSIONS,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            subject_override: None,
        }
    }

    #[must_use]
    pub const fn with_protocol_version(mut self, protocol_version: &'static str) -> Self {
        self.protocol_version = protocol_version;
        self
    }

    #[must_use]
    pub const fn with_max_sessions(mut self, max_sessions: usize) -> Self {
        self.max_sessions = max_sessions;
        self
    }

    #[must_use]
    pub const fn with_max_request_bytes(mut self, max_request_bytes: usize) -> Self {
        self.max_request_bytes = max_request_bytes;
        self
    }

    #[must_use]
    pub fn with_subject_override(mut self, subject: Subject) -> Self {
        self.subject_override = Some(subject);
        self
    }
}

#[cfg(test)]
mod tests {
    use crabgent_core::ModelTarget;
    use secrecy::SecretString;

    use super::*;

    #[test]
    fn new_sets_default_protocol_and_session_cap() {
        let config = McpServerConfig::new(
            SecretString::from("secret-test-token-12345"),
            ModelTarget::id("test-model"),
        );

        assert_eq!(config.protocol_version, MCP_PROTOCOL_VERSION);
        assert_eq!(config.max_sessions, DEFAULT_MAX_SESSIONS);
        assert_eq!(config.max_request_bytes, DEFAULT_MAX_REQUEST_BYTES);
        assert_eq!(config.default_model, ModelTarget::id("test-model"));
    }

    #[test]
    fn setters_override_protocol_and_session_cap() {
        let config = McpServerConfig::new(
            SecretString::from("secret-test-token-12345"),
            ModelTarget::id("test-model"),
        )
        .with_protocol_version("2025-06-18")
        .with_max_sessions(12)
        .with_max_request_bytes(2_048);

        assert_eq!(config.protocol_version, "2025-06-18");
        assert_eq!(config.max_sessions, 12);
        assert_eq!(config.max_request_bytes, 2_048);
    }
}
