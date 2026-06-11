//! Shared session resolution for the compaction hook and the recall tool.
//!
//! Mirrors the `crabgent-tool-cache` resolver pattern: the hook (with
//! `RunCtx`) and the tool (with `ToolCtx`) both derive the same
//! [`crabgent_store::SessionId`] from the run [`Subject`], so they agree on
//! the store key without `ToolCtx` having to carry a session. The namespace
//! is distinct from the tool-cache one so the two crates never alias keys
//! when they happen to share a store.

use std::str::FromStr;
use std::sync::Arc;

use crabgent_core::Subject;
use crabgent_store::SessionId;
use uuid::Uuid;

/// `UUIDv5` namespace for subject-derived compaction session ids. Distinct
/// from the `crabgent-tool-cache` namespace.
const SUBJECT_NAMESPACE: Uuid = Uuid::from_bytes([
    0x7a, 0x11, 0x9c, 0x42, 0x6e, 0x3d, 0x4b, 0x88, 0xb5, 0x21, 0x0c, 0x9f, 0x3e, 0x77, 0xd4, 0x10,
]);

/// Maps a run subject to the session used by both the hook and the tool.
pub type SessionResolver = Arc<dyn Fn(&Subject) -> SessionId + Send + Sync>;

/// The default resolver, wrapping [`default_session_id`]. Shared by the hook
/// and the recall tool so they agree on the store key.
#[must_use]
pub fn default_session_resolver() -> SessionResolver {
    Arc::new(default_session_id)
}

/// Resolve the default compaction session for a subject.
///
/// A UUID-shaped subject id is used directly; any other id maps to a
/// deterministic `UUIDv5` under the compaction namespace.
#[must_use]
pub fn default_session_id(subject: &Subject) -> SessionId {
    SessionId::from_str(subject.id()).unwrap_or_else(|_| {
        SessionId::from_uuid(Uuid::new_v5(&SUBJECT_NAMESPACE, subject.id().as_bytes()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_resolver_deterministic() {
        let subject = Subject::new("alice");
        assert_eq!(default_session_id(&subject), default_session_id(&subject));
    }

    #[test]
    fn distinct_subjects_map_to_distinct_sessions() {
        assert_ne!(
            default_session_id(&Subject::new("alice")),
            default_session_id(&Subject::new("bob"))
        );
    }

    #[test]
    fn uuid_subject_id_is_used_verbatim() {
        let session = SessionId::new();
        let subject = Subject::new(session.to_string());
        assert_eq!(default_session_id(&subject), session);
    }
}
