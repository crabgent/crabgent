//! Persistence-oriented identity types: [`Owner`] and [`ThreadId`].
//!
//! `Subject` (in [`crate::subject`]) answers "who made the call" for
//! authorization. `Owner` answers "to whom does the persisted state belong"
//! and is used by storage layers (sessions, tasks, cron jobs) to scope rows.
//! Decoupling them keeps authorization identity separate from persistence
//! ownership.
//!
//! Both wrap an opaque `String`. The kernel never inspects their contents.
//! Consumers pick whatever shape fits the deployment (user id, channel id,
//! API client name, etc.).

use serde::{Deserialize, Serialize};

use crate::newtype::string_newtype;

/// Stable owner identity for persisted state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Owner(String);

string_newtype!(passthrough_as_ref Owner);

/// Opaque thread identifier for sessions that group multiple turns under one
/// thread (chat thread, support ticket, support reply chain, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThreadId(String);

string_newtype!(passthrough_as_ref ThreadId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_round_trip_via_str_and_string() {
        let a = Owner::new("user-1");
        let b = Owner::from("user-1".to_owned());
        let c: Owner = "user-1".into();
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn owner_display_matches_inner() {
        let o = Owner::new("dm:U123");
        assert_eq!(format!("{o}"), "dm:U123");
        assert_eq!(o.as_str(), "dm:U123");
        assert_eq!(o.as_ref(), "dm:U123");
    }

    #[test]
    fn owner_serde_round_trip_is_transparent() {
        let o = Owner::new("api:client-x");
        let json = serde_json::to_string(&o).expect("serialize");
        assert_eq!(json, "\"api:client-x\"");
        let back: Owner = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(o, back);
    }

    #[test]
    fn thread_id_round_trip_via_str_and_string() {
        let a = ThreadId::new("1700000000.000100");
        let b = ThreadId::from("1700000000.000100".to_owned());
        let c: ThreadId = "1700000000.000100".into();
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn thread_id_display_matches_inner() {
        let t = ThreadId::new("ts:42");
        assert_eq!(format!("{t}"), "ts:42");
        assert_eq!(t.as_str(), "ts:42");
        assert_eq!(t.as_ref(), "ts:42");
    }

    #[test]
    fn thread_id_serde_round_trip_is_transparent() {
        let t = ThreadId::new("thread-42");
        let json = serde_json::to_string(&t).expect("serialize");
        assert_eq!(json, "\"thread-42\"");
        let back: ThreadId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }

    #[test]
    fn distinct_values_compare_unequal() {
        assert_ne!(Owner::new("a"), Owner::new("b"));
        assert_ne!(ThreadId::new("a"), ThreadId::new("b"));
    }

    #[test]
    fn types_are_hashable() {
        use std::collections::HashSet;
        let mut s = HashSet::new();
        s.insert(Owner::new("a"));
        s.insert(Owner::new("a"));
        s.insert(Owner::new("b"));
        assert_eq!(s.len(), 2);
    }
}
