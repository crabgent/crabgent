//! Unique identifier for a single `Kernel::run()` invocation.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A unique identifier for one `Kernel::run()` invocation.
///
/// Callers supply a `RunId` per request. Hooks receive it in every event so
/// that, for example, an injection hook can route out-of-band messages to
/// the correct active run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(Uuid);

impl RunId {
    /// Generate a fresh `UUIDv7`-based `RunId`. Time-ordered for natural sortability.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wrap an existing UUID.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Borrow the inner UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Error returned by `RunId::from_str` for malformed inputs.
#[derive(Debug, thiserror::Error)]
#[error("invalid run id: {0}")]
pub struct ParseRunIdError(uuid::Error);

impl FromStr for RunId {
    type Err = ParseRunIdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self).map_err(ParseRunIdError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_produces_distinct_ids() {
        let a = RunId::new();
        let b = RunId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn default_creates_valid_id() {
        let id = RunId::default();
        assert!(!id.to_string().is_empty());
    }

    #[test]
    fn round_trip_through_string() {
        let id = RunId::new();
        let s = id.to_string();
        let parsed: RunId = s.parse().expect("parse should succeed");
        assert_eq!(id, parsed);
    }

    #[test]
    fn parse_invalid_returns_err() {
        let r: Result<RunId, _> = "not-a-uuid".parse();
        r.expect_err("expected error");
    }

    #[test]
    fn from_uuid_preserves_value() {
        let uuid = Uuid::now_v7();
        let id = RunId::from_uuid(uuid);
        assert_eq!(id.as_uuid(), &uuid);
    }

    #[test]
    fn serde_round_trip() {
        let id = RunId::new();
        let json = serde_json::to_string(&id).expect("serialize");
        let back: RunId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
    }

    #[test]
    fn serde_transparent_uses_plain_uuid_string() {
        let uuid = Uuid::now_v7();
        let id = RunId::from_uuid(uuid);
        let json = serde_json::to_value(&id).expect("serialize");
        assert_eq!(json, serde_json::Value::String(uuid.to_string()));

        let back: RunId = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, id);
    }

    #[test]
    fn display_is_uuid_format() {
        let id = RunId::new();
        let s = id.to_string();
        assert_eq!(s.len(), 36);
        assert_eq!(s.chars().filter(|c| *c == '-').count(), 4);
    }
}
