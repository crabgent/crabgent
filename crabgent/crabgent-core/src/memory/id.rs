//! Identifier for a single memory document.
//!
//! Uses UUID v7 (time-ordered) for natural sortability and collision
//! safety. Mirrors `RunId` in shape and `MemoryId` in serialization
//! (transparent string form on the wire).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for a memory document.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemoryId(Uuid);

impl MemoryId {
    /// Generate a fresh time-ordered id.
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

impl Default for MemoryId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Error returned by `MemoryId::from_str` for malformed inputs.
#[derive(Debug, thiserror::Error)]
#[error("invalid memory id: {0}")]
pub struct ParseMemoryIdError(uuid::Error);

impl FromStr for MemoryId {
    type Err = ParseMemoryIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self).map_err(ParseMemoryIdError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_produces_distinct_ids() {
        let a = MemoryId::new();
        let b = MemoryId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn default_creates_valid_id() {
        let id = MemoryId::default();
        assert!(!id.to_string().is_empty());
    }

    #[test]
    fn round_trip_through_string() {
        let id = MemoryId::new();
        let s = id.to_string();
        let parsed: MemoryId = s.parse().expect("parse should succeed");
        assert_eq!(id, parsed);
    }

    #[test]
    fn parse_invalid_returns_err() {
        let r: Result<MemoryId, _> = "not-a-uuid".parse();
        r.expect_err("expected error");
    }

    #[test]
    fn from_uuid_preserves_value() {
        let uuid = Uuid::now_v7();
        let id = MemoryId::from_uuid(uuid);
        assert_eq!(id.as_uuid(), &uuid);
    }

    #[test]
    fn serde_round_trip_is_transparent_string() {
        let id = MemoryId::new();
        let json = serde_json::to_string(&id).expect("serialize");
        assert!(json.starts_with('"') && json.ends_with('"'));
        let back: MemoryId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
    }

    #[test]
    fn display_is_uuid_format() {
        let id = MemoryId::new();
        let s = id.to_string();
        assert_eq!(s.len(), 36);
        assert_eq!(s.chars().filter(|c| *c == '-').count(), 4);
    }

    #[test]
    fn ids_hash_into_set() {
        use std::collections::HashSet;
        let mut set: HashSet<MemoryId> = HashSet::new();
        let a = MemoryId::new();
        set.insert(a.clone());
        set.insert(a);
        assert_eq!(set.len(), 1);
    }
}
