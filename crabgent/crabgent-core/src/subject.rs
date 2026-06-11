//! Subject identity carried through the kernel for permission decisions.

use std::collections::HashMap;

use thiserror::Error;

/// A subject identity passed to the kernel for permission decisions.
///
/// The kernel itself never inspects `attrs`. The `PolicyHook` interprets
/// attributes according to its own rules (RBAC, capabilities, trust
/// levels, etc.).
#[derive(Debug, Clone)]
pub struct Subject {
    id: String,
    attrs: HashMap<String, String>,
}

/// Error returned when a subject identity is malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("subject id must not be empty or whitespace-only")]
pub struct InvalidSubjectError;

impl Subject {
    /// Create a new subject with no attributes.
    ///
    /// Panics if `id` is empty or whitespace-only. Use [`Self::try_new`] for
    /// fallible input paths.
    #[expect(
        clippy::panic,
        reason = "Subject::new is the infallible convenience API; try_new is available for input paths"
    )]
    pub fn new(id: impl Into<String>) -> Self {
        match Self::try_new(id) {
            Ok(subject) => subject,
            Err(InvalidSubjectError) => {
                panic!("Subject id must not be empty or whitespace-only");
            }
        }
    }

    /// Try to create a new subject with no attributes.
    ///
    /// Empty and whitespace-only ids are rejected because policy decisions
    /// require an explicit caller identity.
    pub fn try_new(id: impl Into<String>) -> Result<Self, InvalidSubjectError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(InvalidSubjectError);
        }
        Ok(Self {
            id,
            attrs: HashMap::new(),
        })
    }

    /// Borrow the subject id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Borrow all subject attributes.
    #[must_use]
    pub const fn attrs(&self) -> &HashMap<String, String> {
        &self.attrs
    }

    /// Add or replace an attribute, returning self for chaining.
    #[must_use]
    pub fn with_attr(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attrs.insert(key.into(), value.into());
        self
    }

    /// Look up an attribute by key.
    #[must_use]
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_empty_attrs() {
        let s = Subject::new("user-1");
        assert_eq!(s.id(), "user-1");
        assert!(s.attrs().is_empty());
    }

    #[test]
    #[should_panic(expected = "Subject id must not be empty or whitespace-only")]
    fn new_panics_on_empty_id() {
        let _ = Subject::new("");
    }

    #[test]
    fn try_new_rejects_empty_id() {
        assert!(matches!(Subject::try_new(""), Err(InvalidSubjectError)));
    }

    #[test]
    fn try_new_rejects_whitespace_only_id() {
        assert!(matches!(Subject::try_new("  "), Err(InvalidSubjectError)));
    }

    #[test]
    fn try_new_accepts_valid_id() {
        let s = Subject::try_new("user-1").expect("valid subject");
        assert_eq!(s.id(), "user-1");
        assert!(s.attrs().is_empty());
    }

    #[test]
    fn with_attr_chains() {
        let s = Subject::new("u")
            .with_attr("role", "admin")
            .with_attr("team", "core");
        assert_eq!(s.attr("role"), Some("admin"));
        assert_eq!(s.attr("team"), Some("core"));
    }

    #[test]
    fn missing_attr_returns_none() {
        let s = Subject::new("u");
        assert_eq!(s.attr("missing"), None);
    }

    #[test]
    fn with_attr_replaces_existing() {
        let s = Subject::new("u")
            .with_attr("role", "admin")
            .with_attr("role", "editor");
        assert_eq!(s.attr("role"), Some("editor"));
    }

    #[test]
    fn id_accepts_string_and_str() {
        let s1 = Subject::new("a".to_string());
        let s2 = Subject::new("a");
        assert_eq!(s1.id(), s2.id());
    }

    #[test]
    fn clone_is_independent() {
        let s1 = Subject::new("u").with_attr("k", "v");
        let s2 = s1.clone().with_attr("k2", "v2");
        assert_eq!(s1.attrs().len(), 1);
        assert_eq!(s2.attrs().len(), 2);
    }
}
