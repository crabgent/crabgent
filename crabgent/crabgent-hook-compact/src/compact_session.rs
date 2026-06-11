//! Persisted-session compaction entry point.

use std::sync::Arc;

use crabgent_core::{Decision, Hook, RunCtx, RunId, Subject};
use crabgent_store::{SessionId, SessionStore, StoreError, Utc};

use crate::hook::CompactHook;
use crate::summary_chain::CompactError;

impl CompactHook {
    /// Compact a persisted session and write the compacted message window back
    /// to `SessionStore::save_messages`.
    pub async fn compact_session(
        &self,
        store: Arc<dyn SessionStore>,
        session_id: SessionId,
        subject: Subject,
    ) -> Result<(), CompactError> {
        let session = store
            .load(&session_id)
            .await
            .map_err(CompactError::Store)?
            .ok_or(CompactError::Store(StoreError::NotFound))?;
        if session.messages.is_empty() {
            return Ok(());
        }

        let ctx = RunCtx::new(RunId::new(), subject);
        // invariant: RunCtx::new installs a fresh OnceLock; set always succeeds.
        ctx.set_session_id(session_id.to_string())
            .expect("session id installs on fresh RunCtx");

        let compacted = match self.pre_compact(&session.messages, &ctx).await {
            Decision::Continue => return Ok(()),
            Decision::Replace(messages) => messages,
            Decision::Deny(reason) => return Err(CompactError::Denied(reason)),
        };

        store
            .save_messages(&session_id, &compacted, Utc::now())
            .await
            .map_err(CompactError::Store)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crabgent_core::{ContentBlock, MemoryScope, Message, Owner, Subject};
    use crabgent_store::memory::MemorySessionStore;
    use crabgent_store::{SessionId, SessionStore, StoreError};
    use crabgent_test_support::{StubProvider, user_msg as user};

    use super::*;

    fn message_text(message: &Message) -> String {
        match message {
            Message::User { content, .. } => content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Message::Assistant { text, .. } | Message::System { content: text } => text.clone(),
            Message::ToolResult { output, .. } => output.to_string(),
            _ => String::new(),
        }
    }

    async fn stored_session(store: &Arc<MemorySessionStore>, messages: Vec<Message>) -> SessionId {
        let mut session = store
            .find_or_create(&Owner::new("u"), None, &MemoryScope::default())
            .await
            .expect("session created");
        session.messages = messages;
        store.save(&session).await.expect("session saved");
        session.id
    }

    #[tokio::test]
    async fn compact_session_loads_messages_runs_compact_saves() {
        let store = Arc::new(MemorySessionStore::default());
        let session_id = stored_session(&store, vec![user("old"), user("latest")]).await;
        let provider = Arc::new(StubProvider::with_text("old summary"));
        let hook = CompactHook::new(Arc::clone(&provider), "summary-model")
            .with_max_messages(1)
            .with_keep_recent_messages(1);
        let store_dyn: Arc<dyn SessionStore> = store.clone();

        hook.compact_session(store_dyn, session_id.clone(), Subject::new("u"))
            .await
            .expect("session compacted");

        let loaded = store
            .load(&session_id)
            .await
            .expect("load succeeds")
            .expect("session exists");
        assert_eq!(provider.captured_requests().len(), 1);
        assert_eq!(loaded.messages.len(), 2);
        assert!(message_text(&loaded.messages[0]).contains("old summary"));
        assert!(message_text(&loaded.messages[1]).contains("latest"));
    }

    #[tokio::test]
    async fn compact_session_no_messages_is_noop() {
        let store = Arc::new(MemorySessionStore::default());
        let session_id = stored_session(&store, Vec::new()).await;
        let provider = Arc::new(StubProvider::with_text("unused"));
        let hook = CompactHook::new(Arc::clone(&provider), "summary-model").with_max_messages(1);
        let store_dyn: Arc<dyn SessionStore> = store.clone();

        hook.compact_session(store_dyn, session_id.clone(), Subject::new("u"))
            .await
            .expect("empty session is a noop");

        let loaded = store
            .load(&session_id)
            .await
            .expect("load succeeds")
            .expect("session exists");
        assert!(loaded.messages.is_empty());
        assert_eq!(provider.captured_requests().len(), 0);
    }

    #[tokio::test]
    async fn compact_session_session_not_found_errors() {
        let store = Arc::new(MemorySessionStore::default());
        let provider = Arc::new(StubProvider::with_text("unused"));
        let hook = CompactHook::new(provider, "summary-model");
        let store_dyn: Arc<dyn SessionStore> = store.clone();

        let err = hook
            .compact_session(store_dyn, SessionId::new(), Subject::new("u"))
            .await
            .expect_err("missing session errors");

        assert!(matches!(err, CompactError::Store(StoreError::NotFound)));
    }

    #[tokio::test]
    async fn compact_session_preserves_continuity_summary() {
        let store = Arc::new(MemorySessionStore::default());
        let session_id = stored_session(&store, vec![user("old"), user("latest")]).await;
        store
            .set_compaction_summary(&session_id, "prior compacted context")
            .await
            .expect("prior summary stored");
        let provider = Arc::new(StubProvider::with_text("new compacted context"));
        let hook = CompactHook::new(Arc::clone(&provider), "summary-model")
            .with_max_messages(1)
            .with_keep_recent_messages(1)
            .with_session_store(Arc::clone(&store));
        let store_dyn: Arc<dyn SessionStore> = store.clone();

        hook.compact_session(store_dyn, session_id.clone(), Subject::new("u"))
            .await
            .expect("session compacted");

        let summary_prompt = {
            let requests = provider.captured_requests();
            assert_eq!(requests.len(), 1);
            requests[0].messages[0].to_string()
        };
        assert!(summary_prompt.contains("prior compacted context"));
        assert_eq!(
            store
                .get_compaction_summary(&session_id)
                .await
                .expect("summary loaded")
                .as_deref(),
            Some("new compacted context"),
        );
    }

    #[tokio::test]
    async fn compact_session_continues_without_rewriting_when_under_threshold() {
        let store = Arc::new(MemorySessionStore::default());
        let session_id = stored_session(&store, vec![user("small")]).await;
        let provider = Arc::new(StubProvider::with_text("unused"));
        let hook = CompactHook::new(Arc::clone(&provider), "summary-model").with_max_messages(10);
        let store_dyn: Arc<dyn SessionStore> = store.clone();

        hook.compact_session(store_dyn, session_id.clone(), Subject::new("u"))
            .await
            .expect("small session continues");

        let loaded = store
            .load(&session_id)
            .await
            .expect("load succeeds")
            .expect("session exists");
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(message_text(&loaded.messages[0]), "small");
        assert_eq!(provider.captured_requests().len(), 0);
    }
}
