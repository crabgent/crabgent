//! Command name newtype.

use std::fmt;
use std::str::FromStr;

use crate::error::CommandNameError;

/// Lowercase command name used for registry lookup.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandName(String);

impl CommandName {
    /// Parse a command name.
    pub fn parse(value: impl Into<String>) -> Result<Self, CommandNameError> {
        let value = value.into();
        validate_name(&value)?;
        Ok(Self(value))
    }

    /// Borrow the command name as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CommandName {
    type Err = CommandNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<&str> for CommandName {
    type Error = CommandNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

fn validate_name(value: &str) -> Result<(), CommandNameError> {
    if value.is_empty() {
        return Err(CommandNameError::Empty);
    }
    if value.chars().any(char::is_whitespace) {
        return Err(CommandNameError::Whitespace);
    }
    let valid = value
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '-' | '_'));
    if valid {
        Ok(())
    } else {
        Err(CommandNameError::InvalidCharacter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty() {
        assert!(matches!(
            CommandName::parse(""),
            Err(CommandNameError::Empty)
        ));
    }

    #[test]
    fn rejects_whitespace() {
        assert!(matches!(
            CommandName::parse("model list"),
            Err(CommandNameError::Whitespace)
        ));
    }

    #[test]
    fn lowercase_only() {
        let name = CommandName::parse("model-list_1").expect("valid lowercase command name");
        assert_eq!(name.as_str(), "model-list_1");
        assert!(matches!(
            CommandName::parse("Model"),
            Err(CommandNameError::InvalidCharacter)
        ));
    }
}
