//! Error type returned by every [`crate::Store`] operation.

use thiserror::Error;

/// Errors returned by store operations.
///
/// Backends map their native errors into these variants so consumers can
/// distinguish recoverable from terminal failures without depending on the
/// concrete backend crate.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The requested item does not exist.
    #[error("not found")]
    NotFound,

    /// A unique constraint was violated (duplicate id or natural key).
    #[error("conflict: {0}")]
    Conflict(String),

    /// A transient failure that callers may retry (lock contention, busy db,
    /// transient network glitch).
    #[error("transient: {0}")]
    Transient(String),

    /// Validation failed for the given input (oversize prompt, malformed
    /// schedule, ...).
    #[error("invalid input: {0}")]
    Invalid(String),

    /// Backend does not support the requested operation.
    #[error("operation not supported by backend")]
    Unsupported,

    /// Serialization or deserialization of a record failed.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Catch-all for backend-specific errors that do not fit any other
    /// variant.
    #[error("backend error: {0}")]
    Backend(String),
}

impl StoreError {
    /// Construct a `Backend` error from any displayable value.
    pub fn backend(value: impl std::fmt::Display) -> Self {
        Self::Backend(value.to_string())
    }

    /// Construct an `Invalid` error from any displayable value.
    pub fn invalid(value: impl std::fmt::Display) -> Self {
        Self::Invalid(value.to_string())
    }

    /// True if this error is worth retrying without changing the request.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    /// Stable, non-sensitive variant label for logs and metrics.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Transient(_) => "transient",
            Self::Invalid(_) => "invalid",
            Self::Unsupported => "unsupported",
            Self::Serialization(_) => "serialization",
            Self::Backend(_) => "backend",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_variant_text() {
        assert_eq!(StoreError::NotFound.to_string(), "not found");
        assert_eq!(
            StoreError::Conflict("dup".into()).to_string(),
            "conflict: dup"
        );
        assert_eq!(
            StoreError::Unsupported.to_string(),
            "operation not supported by backend"
        );
    }

    #[test]
    fn unsupported_display() {
        assert_eq!(
            StoreError::Unsupported.to_string(),
            "operation not supported by backend"
        );
    }

    #[test]
    fn backend_constructor_wraps_display() {
        let err = StoreError::backend("disk full");
        assert!(matches!(err, StoreError::Backend(ref s) if s == "disk full"));
    }

    #[test]
    fn invalid_constructor_wraps_display() {
        let err = StoreError::invalid("limit must be positive");
        assert!(matches!(err, StoreError::Invalid(ref s) if s == "limit must be positive"));
    }

    #[test]
    fn is_transient_only_for_transient_variant() {
        assert!(StoreError::Transient("retry".into()).is_transient());
        assert!(!StoreError::NotFound.is_transient());
        assert!(!StoreError::Backend("x".into()).is_transient());
    }

    #[test]
    fn kind_omits_variant_payload() {
        let err = StoreError::Backend("dsn=postgres://secret".into());

        assert_eq!(err.kind(), "backend");
        assert!(!err.kind().contains("secret"));
    }

    #[test]
    fn serialization_wraps_serde_json_error() {
        let bad: Result<i32, serde_json::Error> = serde_json::from_str("not-json");
        let serde_err = bad.expect_err("expected error");
        let store_err: StoreError = serde_err.into();
        assert!(matches!(store_err, StoreError::Serialization(_)));
    }
}
