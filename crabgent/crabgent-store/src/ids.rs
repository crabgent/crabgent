//! UUIDv7-backed identifiers for sessions, tasks, and cron jobs.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Error returned when a string cannot be parsed into one of the id newtypes.
#[derive(Debug, thiserror::Error)]
#[error("invalid id: {0}")]
pub struct ParseIdError(uuid::Error);

/// Generate a UUIDv7-backed id newtype with its shared impl block.
///
/// The struct declaration, its doc comment, and its `#[serde(transparent)]`
/// attribute stay visible at the call site; only the repeated inherent + trait
/// impls are generated here. Two modes match the two derive shapes used in this
/// module:
/// - `move`: `Clone` only (`SessionId`, `TaskId`, `CronJobId`, `RelationId`).
/// - `copy`: `Clone` + `Copy` (`ArchiveId`).
macro_rules! id_newtype {
    (move $name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        id_newtype!(@impls $name);
    };

    (copy $name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        id_newtype!(@impls $name);
    };

    (@impls $name:ident) => {
        impl $name {
            /// Generate a fresh time-ordered UUIDv7-based id.
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

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = ParseIdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self).map_err(ParseIdError)
            }
        }
    };
}

id_newtype!(copy ArchiveId, "Identifier for a persisted session archive.");
id_newtype!(move SessionId, "Identifier for a persisted session.");
id_newtype!(move TaskId, "Identifier for a background task.");
id_newtype!(move CronJobId, "Identifier for a cron job.");
id_newtype!(move RelationId, "Identifier for a memory relation edge.");
id_newtype!(move GoalId, "Identifier for a thread goal.");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newtypes_generate_distinct_ids() {
        assert_ne!(SessionId::new(), SessionId::new());
        assert_ne!(TaskId::new(), TaskId::new());
        assert_ne!(CronJobId::new(), CronJobId::new());
    }

    #[test]
    fn newtypes_round_trip_via_string() {
        let id = SessionId::new();
        let s = id.to_string();
        let parsed: SessionId = s.parse().expect("parse session id");
        assert_eq!(id, parsed);
    }

    #[test]
    fn parse_invalid_returns_err() {
        let r: Result<TaskId, _> = "not-a-uuid".parse();
        r.expect_err("expected error");
    }

    #[test]
    fn from_uuid_preserves_value() {
        let uuid = Uuid::now_v7();
        let id = CronJobId::from_uuid(uuid);
        assert_eq!(id.as_uuid(), &uuid);
    }

    #[test]
    fn default_is_fresh_uuid() {
        let a = TaskId::default();
        let b = TaskId::default();
        assert_ne!(a, b);
    }

    #[test]
    fn serde_round_trip_is_transparent() {
        let id = CronJobId::new();
        let json = serde_json::to_string(&id).expect("serialize");
        let back: CronJobId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
        assert!(json.starts_with('"'));
    }
}
