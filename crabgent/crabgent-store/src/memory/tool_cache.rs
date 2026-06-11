//! In-memory [`ToolCacheStore`] backed by a `HashMap` behind a `Mutex`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::Utc;

use crate::error::StoreError;
use crate::ids::SessionId;
use crate::records::ToolCacheEntry;
use crate::traits::ToolCacheStore;

type Key = (String, SessionId);

#[derive(Default)]
pub struct MemoryToolCacheStore {
    inner: Mutex<HashMap<Key, ToolCacheEntry>>,
}

impl MemoryToolCacheStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<Key, ToolCacheEntry>>, StoreError> {
        self.inner
            .lock()
            .map_err(|e| StoreError::backend(format!("tool cache mutex poisoned: {e}")))
    }
}

#[async_trait]
impl ToolCacheStore for MemoryToolCacheStore {
    async fn insert(&self, entry: &ToolCacheEntry) -> Result<(), StoreError> {
        let key = (entry.id.clone(), entry.session_id.clone());
        let mut entries = self.lock()?;
        entries.entry(key).or_insert_with(|| entry.clone());
        Ok(())
    }

    async fn get(
        &self,
        id: &str,
        session_id: &SessionId,
    ) -> Result<Option<ToolCacheEntry>, StoreError> {
        let entries = self.lock()?;
        let key = (id.to_owned(), session_id.clone());
        Ok(entries
            .get(&key)
            .filter(|e| e.expires_at > Utc::now())
            .cloned())
    }

    async fn cleanup_expired(&self) -> Result<u64, StoreError> {
        let now = Utc::now();
        let mut entries = self.lock()?;
        let before = entries.len();
        entries.retain(|_, e| e.expires_at > now);
        Ok((before - entries.len()) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_entry(
        id: &str,
        session: &SessionId,
        expires_at: chrono::DateTime<Utc>,
    ) -> ToolCacheEntry {
        ToolCacheEntry {
            id: id.to_owned(),
            session_id: session.clone(),
            tool_name: "bash".into(),
            content: format!("output for {id}"),
            preview: "output...".into(),
            created_at: Utc::now(),
            expires_at,
        }
    }

    #[tokio::test]
    async fn insert_and_get() {
        let store = MemoryToolCacheStore::default();
        let session = SessionId::new();
        let entry = make_entry("c1", &session, Utc::now() + Duration::hours(1));
        store.insert(&entry).await.expect("test result");
        let got = store
            .get("c1", &session)
            .await
            .expect("test result")
            .expect("test result");
        assert_eq!(got.content, "output for c1");
    }

    #[tokio::test]
    async fn get_scoped_by_session() {
        let store = MemoryToolCacheStore::default();
        let s1 = SessionId::new();
        let s2 = SessionId::new();
        let entry = make_entry("c2", &s1, Utc::now() + Duration::hours(1));
        store.insert(&entry).await.expect("test result");
        assert!(store.get("c2", &s2).await.expect("test result").is_none());
    }

    #[tokio::test]
    async fn get_expired_returns_none() {
        let store = MemoryToolCacheStore::default();
        let session = SessionId::new();
        let entry = make_entry("c3", &session, Utc::now() - Duration::hours(1));
        store.insert(&entry).await.expect("test result");
        assert!(
            store
                .get("c3", &session)
                .await
                .expect("test result")
                .is_none()
        );
    }

    #[tokio::test]
    async fn insert_idempotent() {
        let store = MemoryToolCacheStore::default();
        let session = SessionId::new();
        let entry = make_entry("c4", &session, Utc::now() + Duration::hours(1));
        store.insert(&entry).await.expect("test result");
        // Second insert with same id+session is a no-op.
        let mut shadowed = entry.clone();
        shadowed.content = "different".into();
        store.insert(&shadowed).await.expect("test result");
        let got = store
            .get("c4", &session)
            .await
            .expect("test result")
            .expect("test result");
        assert_eq!(got.content, "output for c4");
    }

    #[tokio::test]
    async fn cleanup_expired_removes_only_expired() {
        let store = MemoryToolCacheStore::default();
        let session = SessionId::new();
        let past = Utc::now() - Duration::hours(1);
        let future = Utc::now() + Duration::hours(1);
        store
            .insert(&make_entry("expired1", &session, past))
            .await
            .expect("test result");
        store
            .insert(&make_entry("expired2", &session, past))
            .await
            .expect("test result");
        store
            .insert(&make_entry("valid1", &session, future))
            .await
            .expect("test result");
        let removed = store.cleanup_expired().await.expect("test result");
        assert_eq!(removed, 2);
        assert!(
            store
                .get("valid1", &session)
                .await
                .expect("test result")
                .is_some()
        );
    }
}
