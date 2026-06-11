//! Shared session resolution for tool-cache hook and reader tool.

use std::str::FromStr;
use std::sync::Arc;

use crabgent_core::Subject;
use crabgent_store::SessionId;
use uuid::Uuid;

const SUBJECT_NAMESPACE: Uuid = Uuid::from_bytes([
    0x46, 0x9b, 0x2b, 0x23, 0x98, 0x88, 0x4a, 0x3a, 0xa9, 0x91, 0x5f, 0xf1, 0x2a, 0x4e, 0xe3, 0x52,
]);

/// Maps a run subject to the cache session used by both hook and tool.
pub type SessionResolver = Arc<dyn Fn(&Subject) -> SessionId + Send + Sync>;

pub(crate) fn default_session_resolver() -> SessionResolver {
    Arc::new(default_session_id)
}

/// Resolve the default cache session for a subject.
///
/// UUID-shaped subject ids keep their value. Other ids map to a deterministic
/// `UUIDv5` under a tool-cache namespace so the hook and `cache_read` tool can
/// agree without widening `ToolCtx`.
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
    fn uuid_subject_ids_keep_their_value() {
        let session = SessionId::new();
        let subject = Subject::new(session.to_string());

        assert_eq!(default_session_id(&subject), session);
    }

    #[test]
    fn non_uuid_subject_ids_map_deterministically() {
        let subject = Subject::new("alice");

        assert_eq!(default_session_id(&subject), default_session_id(&subject));
        assert_ne!(
            default_session_id(&subject),
            default_session_id(&Subject::new("bob"))
        );
    }
}
