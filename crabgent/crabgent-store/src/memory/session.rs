//! In-memory [`SessionStore`] backed by a `HashMap` behind a `Mutex`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use crabgent_core::{MemoryScope, Message, Owner, SearchQuery, ThreadId};

use crate::error::StoreError;
use crate::ids::{ArchiveId, SessionId};
use crate::page::Page;
use crate::records::{Session, SessionArchiveEntry, SessionInfo, SessionSearchHit};
use crate::scope_query::ScopeQuery;
use crate::session_support::{
    date_range_contains, new_empty_session, normalized_session_scope, session_identity_scope,
    session_search_text,
};
use crate::traits::SessionStore;

#[derive(Default)]
pub struct MemorySessionStore {
    inner: Mutex<SessionMap>,
    archives: Mutex<ArchiveMap>,
}

type SessionMap = HashMap<SessionId, Session>;
type ArchiveMap = HashMap<SessionId, Vec<SessionArchiveEntry>>;

impl MemorySessionStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, SessionMap>, StoreError> {
        self.inner
            .lock()
            .map_err(|e| StoreError::backend(format!("session mutex poisoned: {e}")))
    }

    fn archive_lock(&self) -> Result<std::sync::MutexGuard<'_, ArchiveMap>, StoreError> {
        self.archives
            .lock()
            .map_err(|e| StoreError::backend(format!("session archive mutex poisoned: {e}")))
    }
}

#[async_trait]
impl SessionStore for MemorySessionStore {
    async fn find_or_create(
        &self,
        owner: &Owner,
        thread: Option<&ThreadId>,
        scope: &MemoryScope,
    ) -> Result<Session, StoreError> {
        let mut sessions = self.lock()?;
        let identity_scope = session_identity_scope(owner, scope);
        let identity_query = ScopeQuery::identity(&identity_scope);
        let existing = sessions
            .values()
            .filter(|s| {
                s.thread.as_ref() == thread && identity_query.matches(&normalized_session_scope(s))
            })
            .max_by_key(|s| s.updated_at)
            .cloned();
        if let Some(session) = existing {
            return Ok(session);
        }
        let mut session = new_empty_session(owner, thread, Utc::now());
        let mut stamped = scope.clone();
        stamped.owner = Some(owner.clone());
        session.scope = stamped;
        sessions.insert(session.id.clone(), session.clone());
        Ok(session)
    }

    async fn load(&self, id: &SessionId) -> Result<Option<Session>, StoreError> {
        let sessions = self.lock()?;
        Ok(sessions.get(id).cloned())
    }

    async fn save(&self, session: &Session) -> Result<(), StoreError> {
        let mut sessions = self.lock()?;
        let mut session = session.clone();
        session.scope.owner = Some(session.owner.clone());
        sessions.insert(session.id.clone(), session);
        Ok(())
    }

    async fn save_messages(
        &self,
        id: &SessionId,
        messages: &[Message],
        updated_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut sessions = self.lock()?;
        let Some(session) = sessions.get_mut(id) else {
            return Err(StoreError::NotFound);
        };
        session.messages = messages.to_vec();
        session.updated_at = updated_at;
        Ok(())
    }

    async fn archive_messages(
        &self,
        session_id: &SessionId,
        messages: &[Message],
        created_at: DateTime<Utc>,
    ) -> Result<ArchiveId, StoreError> {
        if !self.lock()?.contains_key(session_id) {
            return Err(StoreError::NotFound);
        }
        let id = ArchiveId::new();
        let entry = SessionArchiveEntry {
            id,
            session_id: session_id.clone(),
            messages: messages.to_vec(),
            created_at,
        };
        self.archive_lock()?
            .entry(session_id.clone())
            .or_default()
            .push(entry);
        Ok(id)
    }

    async fn list_archives(
        &self,
        session_id: &SessionId,
        page: Page,
    ) -> Result<Vec<SessionArchiveEntry>, StoreError> {
        let archives = self.archive_lock()?;
        let mut entries = archives.get(session_id).cloned().unwrap_or_default();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.created_at));
        Ok(entries
            .into_iter()
            .skip(page.offset)
            .take(page.limit)
            .collect())
    }

    async fn cleanup_old_archives(&self, days: i64) -> Result<u64, StoreError> {
        let cutoff = Utc::now() - Duration::days(days);
        let mut archives = self.archive_lock()?;
        let mut removed = 0;
        for entries in archives.values_mut() {
            let before = entries.len();
            entries.retain(|entry| entry.created_at >= cutoff);
            removed += before - entries.len();
        }
        Ok(removed as u64)
    }

    async fn list(&self, owner: &Owner, page: Page) -> Result<Vec<SessionInfo>, StoreError> {
        let sessions = self.lock()?;
        let mut filtered: Vec<&Session> = sessions.values().filter(|s| &s.owner == owner).collect();
        filtered.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        let infos: Vec<SessionInfo> = filtered
            .into_iter()
            .skip(page.offset)
            .take(page.limit)
            .map(SessionInfo::from)
            .collect();
        Ok(infos)
    }

    async fn cleanup_old(&self, days: i64) -> Result<u64, StoreError> {
        let cutoff = Utc::now() - Duration::days(days);
        let mut sessions = self.lock()?;
        let before = sessions.len();
        sessions.retain(|_, s| s.updated_at >= cutoff);
        Ok((before - sessions.len()) as u64)
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SessionSearchHit>, StoreError> {
        let q_lower = query.query.to_lowercase();
        let sessions = self.lock()?;
        let mut hits: Vec<SessionSearchHit> = sessions
            .values()
            .filter(|s| scope_filter_matches(query, s))
            .filter(|s| date_range_contains(query.since, query.until, s.updated_at))
            .filter_map(|s| build_hit(s, &q_lower))
            .collect();
        hits.sort_by_key(|h| std::cmp::Reverse(h.occurred_at));
        let offset = usize::try_from(query.offset).unwrap_or(usize::MAX);
        let limit = usize::try_from(query.limit).unwrap_or(usize::MAX);
        Ok(hits.into_iter().skip(offset).take(limit).collect())
    }
}

fn scope_filter_matches(query: &SearchQuery, session: &Session) -> bool {
    ScopeQuery::filter(&query.scope).matches(&normalized_session_scope(session))
}

fn build_hit(session: &Session, q_lower: &str) -> Option<SessionSearchHit> {
    let body = session_search_text(session);
    if !q_lower.is_empty() && !body.to_lowercase().contains(q_lower) {
        return None;
    }
    Some(SessionSearchHit {
        session_id: session.id.clone(),
        excerpt: snippet(&body, q_lower),
        score: 1.0,
        occurred_at: session.updated_at,
    })
}

const SNIPPET_BEFORE: usize = 80;
const SNIPPET_AFTER: usize = 120;

fn snippet(text: &str, q_lower: &str) -> String {
    if q_lower.is_empty() {
        return text.chars().take(SNIPPET_BEFORE + SNIPPET_AFTER).collect();
    }
    let lower = text.to_lowercase();
    let Some(pos) = lower.find(q_lower) else {
        return text.chars().take(SNIPPET_BEFORE + SNIPPET_AFTER).collect();
    };
    let start = pos.saturating_sub(SNIPPET_BEFORE);
    let end = (pos + q_lower.len() + SNIPPET_AFTER).min(text.len());
    text.get(start..end).unwrap_or(text).to_owned()
}

#[cfg(test)]
mod tests;
