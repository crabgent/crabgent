//! Agent-name newtype for command session scoping.

use std::fmt;
use std::str::FromStr;

use crate::error::CommandAgentNameError;

/// Agent identity used to align command sessions with kernel sessions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommandAgentName(String);

impl CommandAgentName {
    /// Parse an agent name.
    pub fn parse(value: impl Into<String>) -> Result<Self, CommandAgentNameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CommandAgentNameError::Empty);
        }
        if value.chars().any(char::is_whitespace) {
            return Err(CommandAgentNameError::Whitespace);
        }
        Ok(Self(value))
    }

    /// Borrow the agent name as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandAgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CommandAgentName {
    type Err = CommandAgentNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<&str> for CommandAgentName {
    type Error = CommandAgentNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty() {
        assert!(matches!(
            CommandAgentName::parse(""),
            Err(CommandAgentNameError::Empty)
        ));
    }

    #[test]
    fn rejects_blank() {
        assert!(matches!(
            CommandAgentName::parse("   "),
            Err(CommandAgentNameError::Whitespace)
        ));
    }

    #[test]
    fn rejects_leading_trailing_and_internal_whitespace() {
        for value in [" worker", "worker ", "wer ner"] {
            assert!(matches!(
                CommandAgentName::parse(value),
                Err(CommandAgentNameError::Whitespace)
            ));
        }
    }

    #[test]
    fn allows_unicode_without_whitespace() {
        let name = CommandAgentName::parse("prüfer").expect("valid test agent name");
        assert_eq!(name.as_str(), "prüfer");
    }

    #[test]
    fn rejects_newline() {
        assert!(matches!(
            CommandAgentName::parse("worker\n"),
            Err(CommandAgentNameError::Whitespace)
        ));
    }

    #[test]
    fn parses_and_displays() {
        let name = CommandAgentName::parse("worker").expect("valid test agent name");
        assert_eq!(name.as_str(), "worker");
        assert_eq!(name.to_string(), "worker");
        let via_try = CommandAgentName::try_from("agent_alpha").expect("valid test agent name");
        assert_eq!(via_try.as_str(), "agent_alpha");
    }
}
