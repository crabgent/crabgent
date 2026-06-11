//! Shared session helpers for store backends.

use chrono::{DateTime, Utc};
use crabgent_core::{ContentBlock, MemoryScope, Message, Owner, ThreadId};

use crate::ids::SessionId;
use crate::records::Session;

#[must_use]
pub fn new_empty_session(owner: &Owner, thread: Option<&ThreadId>, now: DateTime<Utc>) -> Session {
    Session {
        id: SessionId::new(),
        owner: owner.clone(),
        scope: session_scope(owner, thread),
        thread: thread.cloned(),
        title: None,
        summary: None,
        compaction_summary: None,
        model_override: None,
        reasoning_effort_override: None,
        messages: Vec::new(),
        created_at: now,
        updated_at: now,
    }
}

#[must_use]
fn session_scope(owner: &Owner, thread: Option<&ThreadId>) -> MemoryScope {
    let mut scope = MemoryScope::for_owner(owner.clone());
    if let Some(thread) = thread {
        scope = scope.with_conv(thread.as_str());
    }
    scope
}

#[must_use]
pub fn normalized_session_scope(session: &Session) -> MemoryScope {
    let mut scope = session.scope.clone();
    scope.owner = Some(session.owner.clone());
    scope
}

#[must_use]
pub fn session_identity_scope(owner: &Owner, scope: &MemoryScope) -> MemoryScope {
    let mut scope = scope.clone();
    scope.owner = Some(owner.clone());
    scope
}

/// Checks whether an instant is within an inclusive time window.
///
/// `DateTime<Utc>` carries sub-second precision, so this helper does not
/// truncate to calendar date boundaries.
#[must_use]
pub fn date_range_contains(
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    at: DateTime<Utc>,
) -> bool {
    since.is_none_or(|s| at >= s) && until.is_none_or(|u| at <= u)
}

#[must_use]
pub fn session_search_text(session: &Session) -> String {
    let mut buf = String::new();
    for msg in &session.messages {
        match msg {
            Message::System { content } => append_chunk(&mut buf, content),
            Message::User { content, .. } => {
                for block in content {
                    if let ContentBlock::Text { text } = block {
                        append_chunk(&mut buf, text);
                    }
                }
            }
            Message::Assistant { text, .. } => append_chunk(&mut buf, text),
            _ => {}
        }
    }
    buf
}

fn append_chunk(buf: &mut String, chunk: &str) {
    if !buf.is_empty() {
        buf.push(' ');
    }
    buf.push_str(chunk);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn empty_session_sets_owner_thread_scope_and_timestamps() {
        let owner = Owner::new("alice");
        let thread = ThreadId::new("topic");
        let now = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .expect("valid test datetime");

        let session = new_empty_session(&owner, Some(&thread), now);

        assert_eq!(session.owner, owner);
        assert_eq!(session.thread.as_ref(), Some(&thread));
        assert_eq!(session.scope.owner.as_ref(), Some(&owner));
        assert_eq!(session.scope.conv.as_deref(), Some("topic"));
        assert_eq!(session.created_at, now);
        assert_eq!(session.updated_at, now);
        assert!(session.messages.is_empty());
    }

    #[test]
    fn session_search_text_flattens_text_messages() {
        let owner = Owner::new("alice");
        let mut session = new_empty_session(&owner, None, Utc::now());
        session.messages = vec![
            Message::System {
                content: "system".into(),
            },
            Message::User {
                content: vec![ContentBlock::Text {
                    text: "user".into(),
                }],
                timestamp: None,
            },
            Message::Assistant {
                text: "assistant".into(),
                tool_calls: Vec::new(),
            },
        ];

        assert_eq!(session_search_text(&session), "system user assistant");
    }

    #[test]
    fn date_range_contains_is_inclusive() {
        let at = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .expect("valid test datetime");

        assert!(date_range_contains(Some(at), Some(at), at));
        assert!(!date_range_contains(
            Some(at + chrono::Duration::seconds(1)),
            None,
            at,
        ));
        assert!(!date_range_contains(
            None,
            Some(at - chrono::Duration::seconds(1)),
            at,
        ));
    }
}
