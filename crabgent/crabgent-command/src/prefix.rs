//! Command-prefix newtype.

use std::fmt;
use std::str::FromStr;

use crate::error::CommandPrefixError;

/// Text prefix that marks an inbound message as a command.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommandPrefix(String);

impl CommandPrefix {
    /// Parse a prefix.
    pub fn parse(value: impl Into<String>) -> Result<Self, CommandPrefixError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CommandPrefixError::Empty);
        }
        if value.chars().any(char::is_whitespace) {
            return Err(CommandPrefixError::Whitespace);
        }
        Ok(Self(value))
    }

    /// Borrow the prefix as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for CommandPrefix {
    fn default() -> Self {
        Self("/".to_owned())
    }
}

impl fmt::Display for CommandPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CommandPrefix {
    type Err = CommandPrefixError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<&str> for CommandPrefix {
    type Error = CommandPrefixError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_whitespace() {
        assert!(matches!(
            CommandPrefix::parse("/ bot"),
            Err(CommandPrefixError::Whitespace)
        ));
    }

    #[test]
    fn rejects_empty() {
        assert!(matches!(
            CommandPrefix::parse(""),
            Err(CommandPrefixError::Empty)
        ));
    }

    #[test]
    fn default_is_slash() {
        assert_eq!(CommandPrefix::default().as_str(), "/");
    }

    #[test]
    fn parses_custom_prefix_and_displays() {
        let prefix: CommandPrefix = "!".parse().expect("valid prefix");
        assert_eq!(prefix.as_str(), "!");
        assert_eq!(prefix.to_string(), "!");
        let via_try = CommandPrefix::try_from("#").expect("valid prefix");
        assert_eq!(via_try.as_str(), "#");
    }
}
