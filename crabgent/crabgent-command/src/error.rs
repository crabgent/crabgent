//! Error types for command dispatch.

use crabgent_core::error::ToolError;
use crabgent_store::StoreError;
use thiserror::Error;

/// Invalid command-name input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandNameError {
    #[error("command name must not be empty")]
    Empty,
    #[error("command name must not contain whitespace")]
    Whitespace,
    #[error("command name must be lowercase ASCII with optional digits, dot, dash, or underscore")]
    InvalidCharacter,
}

/// Invalid command-prefix input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandPrefixError {
    #[error("command prefix must not be empty")]
    Empty,
    #[error("command prefix must not contain whitespace")]
    Whitespace,
}

/// Invalid command agent-name input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandAgentNameError {
    #[error("command agent name must not be empty")]
    Empty,
    #[error("command agent name must not contain whitespace")]
    Whitespace,
}

/// Command framework error.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CommandError {
    #[error("invalid command name: {0}")]
    InvalidName(#[from] CommandNameError),
    #[error("invalid command prefix: {0}")]
    InvalidPrefix(#[from] CommandPrefixError),
    #[error("invalid command agent name: {0}")]
    InvalidAgentName(#[from] CommandAgentNameError),
    #[error("duplicate command registration: {0}")]
    DuplicateRegistration(String),
    #[error("command registry must not be empty")]
    EmptyRegistry,
    #[error("command dispatch must be composed inside mandatory channel gates")]
    InvalidComposition,
    #[error("invalid command args: {0}")]
    InvalidArgs(String),
    #[error("policy denied command action: {0}")]
    PermissionDenied(String),
    #[error("command execution failed: {0}")]
    Execution(String),
    #[error("tool execution failed: {0}")]
    Tool(#[from] ToolError),
    #[error("store operation failed: {0}")]
    Store(#[from] StoreError),
}

impl CommandError {
    /// User-safe reply text for command failures.
    ///
    /// Policy-deny reasons are forwarded verbatim because the
    /// `PolicyHook` author owns the wording end-to-end (see
    /// `security.md::Policy-Deny-Reason-Handling`). All other arms
    /// resolve to a constant string so the assistant reply cannot
    /// echo user-typed command text or internal context back to the
    /// channel.
    #[must_use]
    pub fn safe_reply(&self) -> String {
        match self {
            Self::PermissionDenied(reason) => reason.clone(),
            Self::InvalidName(_)
            | Self::InvalidPrefix(_)
            | Self::InvalidAgentName(_)
            | Self::DuplicateRegistration(_)
            | Self::EmptyRegistry
            | Self::InvalidComposition
            | Self::InvalidArgs(_) => "command rejected".to_owned(),
            Self::Execution(_) | Self::Tool(_) | Self::Store(_) => "command failed".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crabgent_core::ToolError;
    use crabgent_store::StoreError;

    use super::*;

    #[test]
    fn permission_deny_safe_reply_uses_policy_reason() {
        let err = CommandError::PermissionDenied("safe policy reason".to_owned());
        assert_eq!(err.safe_reply(), "safe policy reason");
    }

    #[test]
    fn validation_safe_reply_is_generic_and_omits_user_input() {
        let err = CommandError::InvalidArgs("missing id 'secret-prompt-text'".to_owned());
        assert_eq!(err.safe_reply(), "command rejected");
        assert!(!err.safe_reply().contains("secret-prompt-text"));
    }

    #[test]
    fn invalid_name_safe_reply_is_generic() {
        let err = CommandError::InvalidName(CommandNameError::Whitespace);
        assert_eq!(err.safe_reply(), "command rejected");
    }

    #[test]
    fn invalid_agent_name_safe_reply_is_generic() {
        let err = CommandError::InvalidAgentName(CommandAgentNameError::Empty);
        assert_eq!(err.safe_reply(), "command rejected");
    }

    #[test]
    fn execution_tool_and_store_errors_use_generic_safe_reply() {
        let errors = [
            CommandError::Execution("internal path".to_owned()),
            CommandError::Tool(ToolError::Execution("tool detail".to_owned())),
            CommandError::Store(StoreError::backend("backend detail")),
        ];
        for err in errors {
            assert_eq!(err.safe_reply(), "command failed");
        }
    }
}
