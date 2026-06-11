use std::fmt;
use std::num::NonZeroUsize;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use crabgent_core::Subject;
use lru::LruCache;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::McpServerError;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct McpSessionId(Uuid);

impl McpSessionId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn parse(value: &str) -> Result<Self, McpServerError> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_err| McpServerError::InvalidParams("invalid MCP session id".into()))
    }

    #[must_use]
    pub fn as_str(&self) -> String {
        self.0.to_string()
    }

    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for McpSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for McpSessionId {
    type Err = McpServerError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Default for McpSessionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct McpSessionEntry {
    pub id: McpSessionId,
    pub created_at: DateTime<Utc>,
    pub subject: Subject,
    pub cancel_token: CancellationToken,
}

impl McpSessionEntry {
    #[must_use]
    pub fn new(id: McpSessionId, subject: Subject) -> Self {
        Self {
            id,
            created_at: Utc::now(),
            subject,
            cancel_token: CancellationToken::new(),
        }
    }
}

#[derive(Debug)]
pub struct McpSessionRegistry {
    max_sessions: usize,
    sessions: Mutex<LruCache<McpSessionId, McpSessionEntry>>,
    #[cfg(test)]
    get_call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl McpSessionRegistry {
    pub(crate) fn new(max_sessions: usize) -> Result<Self, McpServerError> {
        let capacity = NonZeroUsize::new(max_sessions).ok_or_else(|| {
            McpServerError::InvalidRequest("max_sessions must be greater than zero".into())
        })?;

        Ok(Self {
            max_sessions,
            sessions: Mutex::new(LruCache::new(capacity)),
            #[cfg(test)]
            get_call_count: std::sync::Arc::default(),
        })
    }

    pub async fn create(&self, subject: Subject) -> Result<McpSessionId, McpServerError> {
        let mut sessions = self.sessions.lock().await;
        let id = next_unused_session_id(&sessions);
        let entry = McpSessionEntry::new(id.clone(), subject);

        if let Some((_evicted_id, evicted_entry)) = sessions.push(id.clone(), entry) {
            evicted_entry.cancel_token.cancel();
        }

        Ok(id)
    }

    pub async fn get(&self, id: &McpSessionId) -> Option<McpSessionEntry> {
        let mut sessions = self.sessions.lock().await;
        let entry = sessions.get(id).cloned();
        #[cfg(test)]
        self.get_call_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        entry
    }

    pub async fn remove(&self, id: &McpSessionId) -> Option<McpSessionEntry> {
        let mut sessions = self.sessions.lock().await;
        let entry = sessions.pop(id)?;
        entry.cancel_token.cancel();
        Some(entry)
    }

    pub async fn len(&self) -> usize {
        self.sessions.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.sessions.lock().await.is_empty()
    }

    pub(crate) const fn max_sessions(&self) -> usize {
        self.max_sessions
    }
}

#[cfg(test)]
impl McpSessionRegistry {
    pub(crate) fn get_call_count(&self) -> usize {
        self.get_call_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

fn next_unused_session_id(sessions: &LruCache<McpSessionId, McpSessionEntry>) -> McpSessionId {
    loop {
        let id = McpSessionId::new();
        if !sessions.contains(&id) {
            return id;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn subject() -> Subject {
        Subject::new("mcp-test-user")
    }

    #[test]
    fn session_create_returns_uuidv7() {
        let id = McpSessionId::new();

        assert_eq!(id.as_uuid().get_version_num(), 7);
        assert_eq!(id.to_string(), id.as_str());
    }

    #[test]
    fn session_parse_round_trips_display_value() {
        let id = McpSessionId::new();
        let parsed = id
            .to_string()
            .parse::<McpSessionId>()
            .expect("displayed session id parses");

        assert_eq!(parsed, id);
    }

    #[tokio::test]
    async fn session_get_unknown_returns_none() {
        let registry = McpSessionRegistry::new(2).expect("positive session capacity is valid");
        let unknown = McpSessionId::new();

        assert!(registry.get(&unknown).await.is_none());
    }

    #[tokio::test]
    async fn session_remove_triggers_token_cancel() {
        let registry = McpSessionRegistry::new(2).expect("positive session capacity is valid");
        let id = registry
            .create(subject())
            .await
            .expect("session is created");
        let entry = registry.get(&id).await.expect("session can be read");
        let token = entry.cancel_token.clone();

        let removed = registry
            .remove(&id)
            .await
            .expect("existing session can be removed");

        assert_eq!(removed.id, id);
        assert!(token.is_cancelled());
        assert!(registry.get(&id).await.is_none());
    }

    #[tokio::test]
    async fn session_overflow_evicts_oldest_with_token_cancel() {
        let registry = McpSessionRegistry::new(1).expect("positive session capacity is valid");
        let first = registry.create(subject()).await.expect("first session");
        let first_entry = registry
            .get(&first)
            .await
            .expect("first session is readable");
        let first_token = first_entry.cancel_token.clone();

        let second = registry.create(subject()).await.expect("second session");

        assert!(first_token.is_cancelled());
        assert!(registry.get(&first).await.is_none());
        assert!(registry.get(&second).await.is_some());
        assert_eq!(registry.len().await, 1);
        assert!(!registry.is_empty().await);
    }

    #[tokio::test]
    async fn session_create_returns_unique_ids() {
        let registry = McpSessionRegistry::new(128).expect("positive session capacity is valid");
        let mut ids = HashSet::new();

        for _ in 0..100 {
            let id = registry
                .create(subject())
                .await
                .expect("session is created");
            assert!(ids.insert(id));
        }

        assert_eq!(ids.len(), 100);
        assert_eq!(registry.len().await, 100);
    }

    #[test]
    fn registry_rejects_zero_capacity() {
        let error = McpSessionRegistry::new(0).expect_err("zero session capacity must fail");

        assert!(matches!(error, McpServerError::InvalidRequest(_)));
        assert!(error.to_string().contains("max_sessions"));
    }

    #[test]
    fn registry_tracks_capacity() {
        let registry = McpSessionRegistry::new(3).expect("positive session capacity is valid");

        assert_eq!(registry.max_sessions(), 3);
    }
}
