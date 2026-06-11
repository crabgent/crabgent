//! Open `snake_case` label for a [`crate::records::MemoryRelation`] edge.
//!
//! Relation types are open: any `snake_case` string is a valid label, so the
//! graph layer never grows a closed enum the way [`crabgent_memory::MemoryClass`]
//! does. The newtype centralizes the `snake_case` invariant once instead of
//! re-validating at each producer (tool args, consolidation writes, backend
//! rows). Agent-supplied labels go through [`RelationType::new`]; consolidation
//! uses the infallible constructors so its writes need no `unwrap`/`expect`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Maximum length of a relation type label, in bytes.
pub const MAX_RELATION_TYPE_LEN: usize = 64;

/// Reason a string is not a valid [`RelationType`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RelationTypeError {
    /// The label was empty.
    #[error("relation type must not be empty")]
    Empty,
    /// The label exceeded [`MAX_RELATION_TYPE_LEN`].
    #[error("relation type exceeds {MAX_RELATION_TYPE_LEN} chars")]
    TooLong,
    /// The first character was not a lowercase ASCII letter.
    #[error("relation type must start with a lowercase letter a-z, got {0:?}")]
    InvalidStart(char),
    /// A character outside `[a-z0-9_]` appeared after the first.
    #[error("relation type contains invalid char {0:?}; allowed: a-z, 0-9, _")]
    InvalidChar(char),
}

/// An open `snake_case` relation label, e.g. `supports`, `derived_from`.
///
/// Validated to match `^[a-z][a-z0-9_]*$` with a length cap of
/// [`MAX_RELATION_TYPE_LEN`]. Serializes transparently as a string and
/// re-validates on deserialize so a label loaded from disk or a wire payload
/// cannot bypass the invariant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelationType(String);

impl RelationType {
    /// Validate and wrap an agent- or caller-supplied label.
    ///
    /// # Errors
    /// Returns [`RelationTypeError`] when the label is empty, too long, does
    /// not start with `a-z`, or contains a character outside `[a-z0-9_]`.
    pub fn new(value: impl Into<String>) -> Result<Self, RelationTypeError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RelationTypeError::Empty);
        }
        if value.len() > MAX_RELATION_TYPE_LEN {
            return Err(RelationTypeError::TooLong);
        }
        let mut chars = value.chars();
        let first = chars.next().ok_or(RelationTypeError::Empty)?;
        if !first.is_ascii_lowercase() {
            return Err(RelationTypeError::InvalidStart(first));
        }
        for ch in chars {
            if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_') {
                return Err(RelationTypeError::InvalidChar(ch));
            }
        }
        Ok(Self(value))
    }

    /// Borrow the label as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `derived_from`: a consolidated fact was extracted from a source memory.
    #[must_use]
    pub fn derived_from() -> Self {
        Self("derived_from".to_owned())
    }

    /// `supports`: a source memory reinforces an existing fact.
    #[must_use]
    pub fn supports() -> Self {
        Self("supports".to_owned())
    }

    /// `contradicts`: two facts were judged to conflict yet both kept.
    #[must_use]
    pub fn contradicts() -> Self {
        Self("contradicts".to_owned())
    }

    /// `supersedes`: a newer fact replaced the contents of an older one.
    #[must_use]
    pub fn supersedes() -> Self {
        Self("supersedes".to_owned())
    }
}

impl fmt::Display for RelationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for RelationType {
    type Error = RelationTypeError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RelationType> for String {
    fn from(value: RelationType) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_snake_case_labels() {
        for label in [
            "derived_from",
            "supports",
            "a",
            "a1",
            "foo_bar_baz",
            "x_9_y",
        ] {
            assert!(RelationType::new(label).is_ok(), "rejected {label}");
        }
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(RelationType::new(""), Err(RelationTypeError::Empty));
    }

    #[test]
    fn rejects_uppercase_start() {
        assert_eq!(
            RelationType::new("Foo"),
            Err(RelationTypeError::InvalidStart('F'))
        );
    }

    #[test]
    fn rejects_digit_start() {
        assert_eq!(
            RelationType::new("1x"),
            Err(RelationTypeError::InvalidStart('1'))
        );
    }

    #[test]
    fn rejects_underscore_start() {
        assert_eq!(
            RelationType::new("_x"),
            Err(RelationTypeError::InvalidStart('_'))
        );
    }

    #[test]
    fn rejects_invalid_chars() {
        assert_eq!(
            RelationType::new("a-b"),
            Err(RelationTypeError::InvalidChar('-'))
        );
        assert_eq!(
            RelationType::new("a b"),
            Err(RelationTypeError::InvalidChar(' '))
        );
        assert_eq!(
            RelationType::new("aB"),
            Err(RelationTypeError::InvalidChar('B'))
        );
    }

    #[test]
    fn rejects_too_long() {
        let long = "a".repeat(MAX_RELATION_TYPE_LEN + 1);
        assert_eq!(RelationType::new(long), Err(RelationTypeError::TooLong));
        let max = "a".repeat(MAX_RELATION_TYPE_LEN);
        RelationType::new(max).expect("max-length label is valid");
    }

    #[test]
    fn well_known_constructors_are_valid() {
        for relation in [
            RelationType::derived_from(),
            RelationType::supports(),
            RelationType::contradicts(),
            RelationType::supersedes(),
        ] {
            assert_eq!(
                RelationType::new(relation.as_str()).expect("well-known is valid"),
                relation
            );
        }
    }

    #[test]
    fn serde_round_trips_as_plain_string() {
        let relation = RelationType::supersedes();
        let json = serde_json::to_string(&relation).expect("serialize");
        assert_eq!(json, "\"supersedes\"");
        let back: RelationType = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(relation, back);
    }

    #[test]
    fn serde_rejects_invalid_label() {
        let err = serde_json::from_str::<RelationType>("\"Bad-Label\"");
        assert!(err.is_err(), "invalid label must not deserialize");
    }
}
