//! [`SessionManager`]: imperative API for callers that drive sessions
//! themselves.
//!
//! Wraps a [`SessionStore`] and exposes three async methods that mirror a
//! conversation lifecycle: `open` (find existing or create fresh),
//! `append` (push a message and persist), `close` (final save).
//!
//! For automatic persistence wired to kernel events, prefer
//! [`crate::hook::SessionPersistHook`].

use std::sync::Arc;

use chrono::Utc;
use crabgent_core::{MemoryScope, Message};
use crabgent_store::error::StoreError;
use crabgent_store::records::Session;
use crabgent_store::traits::SessionStore;
use crabgent_store::{Owner, ThreadId};

/// Imperative session lifecycle helper.
///
/// Generic over any [`SessionStore`] backend. Cheap to clone (the inner
/// store is shared via `Arc`).
pub struct SessionManager<S: SessionStore> {
    store: Arc<S>,
}

impl<S: SessionStore> Clone for SessionManager<S> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
        }
    }
}

impl<S: SessionStore> SessionManager<S> {
    /// Wrap a store in a session manager.
    #[must_use]
    pub const fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    /// Borrow the underlying store.
    #[must_use]
    pub const fn store(&self) -> &Arc<S> {
        &self.store
    }

    /// Open a session for the `(owner, thread, scope)` tuple. Returns the
    /// most recent session matching the tuple, or creates a fresh empty
    /// one. `scope.owner` is ignored for matching: the separate `owner`
    /// argument is authoritative.
    pub async fn open(
        &self,
        owner: &Owner,
        thread: Option<&ThreadId>,
        scope: &MemoryScope,
    ) -> Result<Session, StoreError> {
        self.store.find_or_create(owner, thread, scope).await
    }

    /// Push `message` onto `session.messages`, bump `updated_at`, and
    /// persist. Mutates the in-memory session struct so callers can keep
    /// using it without reloading.
    pub async fn append(&self, session: &mut Session, message: Message) -> Result<(), StoreError> {
        session.messages.push(message);
        session.updated_at = Utc::now();
        self.store.save(session).await
    }

    /// Final save. Use after the conversation is done so any
    /// metadata mutation (title, summary, ...) lands.
    pub async fn close(&self, session: &Session) -> Result<(), StoreError> {
        self.store.save(session).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::ContentBlock;
    use crabgent_store::memory::MemorySessionStore;
    use crabgent_test_support::user_msg;

    fn manager() -> (SessionManager<MemorySessionStore>, Arc<MemorySessionStore>) {
        let store = Arc::new(MemorySessionStore::default());
        (SessionManager::new(Arc::clone(&store)), store)
    }

    #[tokio::test]
    async fn open_creates_fresh_session_when_none_exists() {
        let (mgr, _store) = manager();
        let session = mgr
            .open(&Owner::new("u"), None, &MemoryScope::default())
            .await
            .expect("test result");
        assert_eq!(session.owner, Owner::new("u"));
        assert!(session.thread.is_none());
        assert!(session.messages.is_empty());
    }

    #[tokio::test]
    async fn open_returns_existing_session_for_same_owner() {
        let (mgr, _store) = manager();
        let a = mgr
            .open(&Owner::new("u"), None, &MemoryScope::default())
            .await
            .expect("test result");
        let b = mgr
            .open(&Owner::new("u"), None, &MemoryScope::default())
            .await
            .expect("test result");
        assert_eq!(a.id, b.id);
    }

    #[tokio::test]
    async fn open_distinguishes_threads() {
        let (mgr, _store) = manager();
        let a = mgr
            .open(&Owner::new("u"), None, &MemoryScope::default())
            .await
            .expect("test result");
        let thread = ThreadId::new("t1");
        let b = mgr
            .open(&Owner::new("u"), Some(&thread), &MemoryScope::default())
            .await
            .expect("test result");
        assert_ne!(a.id, b.id);
    }

    #[tokio::test]
    async fn append_persists_message_and_bumps_updated_at() {
        let (mgr, store) = manager();
        let mut session = mgr
            .open(&Owner::new("u"), None, &MemoryScope::default())
            .await
            .expect("test result");
        let before = session.updated_at;
        mgr.append(&mut session, user_msg("hi"))
            .await
            .expect("test result");
        assert_eq!(session.messages.len(), 1);
        assert!(session.updated_at >= before);
        let loaded = store
            .load(&session.id)
            .await
            .expect("test result")
            .expect("test result");
        assert_eq!(loaded.messages.len(), 1);
    }

    #[tokio::test]
    async fn append_multiple_messages_keeps_order() {
        let (mgr, _store) = manager();
        let mut session = mgr
            .open(&Owner::new("u"), None, &MemoryScope::default())
            .await
            .expect("test result");
        mgr.append(&mut session, user_msg("first"))
            .await
            .expect("test result");
        mgr.append(&mut session, user_msg("second"))
            .await
            .expect("test result");
        mgr.append(&mut session, user_msg("third"))
            .await
            .expect("test result");
        assert_eq!(session.messages.len(), 3);
        if let Message::User { content, .. } = &session.messages[0]
            && let ContentBlock::Text { text } = &content[0]
        {
            assert_eq!(text, "first");
        }
    }

    #[tokio::test]
    async fn close_persists_metadata_changes() {
        let (mgr, store) = manager();
        let mut session = mgr
            .open(&Owner::new("u"), None, &MemoryScope::default())
            .await
            .expect("test result");
        session.title = Some("topic".into());
        mgr.close(&session).await.expect("test result");
        let loaded = store
            .load(&session.id)
            .await
            .expect("test result")
            .expect("test result");
        assert_eq!(loaded.title.as_deref(), Some("topic"));
    }

    #[tokio::test]
    async fn store_accessor_returns_shared_arc() {
        let (mgr, _store) = manager();
        let a = mgr.store();
        let b = mgr.store();
        assert!(Arc::ptr_eq(a, b));
    }

    #[tokio::test]
    async fn clone_shares_underlying_store() {
        let (mgr, store) = manager();
        let cloned = mgr.clone();
        let session = mgr
            .open(&Owner::new("u"), None, &MemoryScope::default())
            .await
            .expect("test result");
        let loaded = cloned.store().load(&session.id).await.expect("test result");
        assert!(loaded.is_some());
        // Confirm Arc count went up via the explicit external handle.
        let _keep = Arc::clone(&store);
    }
}
