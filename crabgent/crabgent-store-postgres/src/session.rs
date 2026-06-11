//! Postgres session sub-store.

mod archive;
#[cfg(any(test, debug_assertions))]
pub mod pause_after_select_miss;
mod query;
mod save_messages;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use std::str::FromStr;

use crabgent_core::{MemoryScope, Message, Owner, ReasoningEffort, SearchQuery, ThreadId};
use crabgent_store::session_support::{
    new_empty_session, normalized_session_scope, session_search_text,
};
use crabgent_store::{
    ArchiveId, Page, Session, SessionArchiveEntry, SessionId, SessionInfo, SessionSearchHit,
    SessionStore, StoreError,
};
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::retry::retry_transient;
use query::{list_recent, search_messages, select_by_scope, truncate_excerpt};

const COLS: &str = "id, owner, scope_channel, scope_conv, scope_agent, scope_kind, \
                    thread, title, summary, compaction_summary, model_override, \
                    reasoning_effort_override, messages, created_at, updated_at";

/// Postgres implementation of `SessionStore`.
#[derive(Clone)]
pub struct PostgresSessionStore {
    pub(crate) pool: PgPool,
}

impl PostgresSessionStore {
    /// Create a session sub-store from a shared pool.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Borrow the shared sqlx pool.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[derive(FromRow)]
struct SessionRow {
    id: Uuid,
    owner: String,
    scope_channel: Option<String>,
    scope_conv: Option<String>,
    scope_agent: Option<String>,
    scope_kind: Option<String>,
    thread: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    compaction_summary: Option<String>,
    model_override: Option<String>,
    reasoning_effort_override: Option<String>,
    messages: Json<Vec<Message>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct SessionInfoRow {
    id: Uuid,
    owner: String,
    thread: Option<String>,
    title: Option<String>,
    message_count: i32,
    has_summary: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct SessionSearchRow {
    session_id: Uuid,
    excerpt: String,
    score: f64,
    occurred_at: DateTime<Utc>,
}

impl TryFrom<SessionRow> for Session {
    type Error = StoreError;

    fn try_from(row: SessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: SessionId::from_uuid(row.id),
            owner: Owner::new(row.owner.clone()),
            scope: MemoryScope {
                owner: Some(Owner::new(row.owner)),
                channel: row.scope_channel,
                conv: row.scope_conv,
                agent: row.scope_agent,
                kind: row.scope_kind,
            },
            thread: row.thread.map(ThreadId::new),
            title: row.title,
            summary: row.summary,
            compaction_summary: row.compaction_summary,
            model_override: row.model_override,
            reasoning_effort_override: row
                .reasoning_effort_override
                .map(|effort| ReasoningEffort::from_str(&effort).map_err(StoreError::invalid))
                .transpose()?,
            messages: row.messages.0,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

impl From<SessionInfoRow> for SessionInfo {
    fn from(row: SessionInfoRow) -> Self {
        Self {
            id: SessionId::from_uuid(row.id),
            owner: Owner::new(row.owner),
            thread: row.thread.map(ThreadId::new),
            title: row.title,
            message_count: usize::try_from(row.message_count).unwrap_or(0),
            has_summary: row.has_summary,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<SessionSearchRow> for SessionSearchHit {
    fn from(row: SessionSearchRow) -> Self {
        Self {
            session_id: SessionId::from_uuid(row.session_id),
            excerpt: truncate_excerpt(&row.excerpt),
            score: row.score,
            occurred_at: row.occurred_at,
        }
    }
}

#[async_trait]
impl SessionStore for PostgresSessionStore {
    async fn find_or_create(
        &self,
        owner: &Owner,
        thread: Option<&ThreadId>,
        scope: &MemoryScope,
    ) -> Result<Session, StoreError> {
        let thread_s = thread.map(|t| t.as_str().to_owned());
        let channel = scope.channel.clone();
        let conv = scope.conv.clone();
        let agent = scope.agent.clone();
        let kind = scope.kind.clone();

        if let Some(row) = select_by_scope(&self.pool, owner, thread, scope).await? {
            return row.try_into();
        }

        #[cfg(any(test, debug_assertions))]
        pause_after_select_miss::pause_after_select_miss_if_configured(owner).await;

        // Miss: race-safe insert. The migration
        // `20260521000001_session_scope_unique_index.sql` adds a
        // `NULLS NOT DISTINCT` unique index on the scope tuple. Two concurrent
        // misses both attempt INSERT; one wins, the other receives DO NOTHING
        // and re-SELECTs the winner row.
        let mut session = new_empty_session(owner, thread, Utc::now());
        let mut stamped = scope.clone();
        stamped.owner = Some(owner.clone());
        session.scope = stamped;
        let search_body = session_search_text(&session);
        let messages = Json(session.messages.clone());

        // `retry_transient` here guards against 40001/40P01/I/O only. A
        // 23505 unique-violation on the scope tuple cannot reach this layer
        // because `ON CONFLICT (...) DO NOTHING` suppresses it inside
        // Postgres and `fetch_optional` simply returns `None`, which we
        // resolve via the re-SELECT below.
        let inserted = retry_transient("session.find_or_create.insert", || async {
            sqlx::query_as::<_, SessionRow>(sqlx::AssertSqlSafe(format!(
                "INSERT INTO sessions (id, owner, scope_channel, scope_conv, scope_agent, \
                 scope_kind, thread, title, summary, compaction_summary, model_override, \
                 reasoning_effort_override, messages, search_body, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
                 ON CONFLICT (owner, thread, scope_channel, scope_conv, scope_agent, scope_kind) \
                 DO NOTHING \
                 RETURNING {COLS}"
            )))
            .bind(session.id.as_uuid())
            .bind(owner.as_str())
            .bind(channel.as_deref())
            .bind(conv.as_deref())
            .bind(agent.as_deref())
            .bind(kind.as_deref())
            .bind(thread_s.as_deref())
            .bind(session.title.as_deref())
            .bind(session.summary.as_deref())
            .bind(session.compaction_summary.as_deref())
            .bind(session.model_override.as_deref())
            .bind(
                session
                    .reasoning_effort_override
                    .map(ReasoningEffort::as_str),
            )
            .bind(&messages)
            .bind(&search_body)
            .bind(session.created_at)
            .bind(session.updated_at)
            .fetch_optional(&self.pool)
            .await
        })
        .await?;

        if let Some(row) = inserted {
            return row.try_into();
        }

        // Race lost: the conflicting concurrent insert already wrote the row.
        // The unique index guarantees the SELECT below finds exactly that row.
        let row = select_by_scope(&self.pool, owner, thread, scope)
            .await?
            .ok_or_else(|| {
                StoreError::backend(
                    "session.find_or_create: conflict but re-select returned no row",
                )
            })?;
        row.try_into()
    }

    async fn load(&self, id: &SessionId) -> Result<Option<Session>, StoreError> {
        let row = retry_transient("session.load", || async {
            sqlx::query_as::<_, SessionRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM sessions WHERE id = $1"
            )))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    async fn save(&self, session: &Session) -> Result<(), StoreError> {
        let scope = normalized_session_scope(session);
        let search_body = session_search_text(session);
        let messages = Json(session.messages.clone());
        retry_transient("session.save", || async {
            sqlx::query(
                "INSERT INTO sessions (id, owner, scope_channel, scope_conv, scope_agent, \
                 scope_kind, thread, title, summary, compaction_summary, model_override, \
                 reasoning_effort_override, messages, search_body, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
                 ON CONFLICT(id) DO UPDATE SET \
                 owner = excluded.owner, scope_channel = excluded.scope_channel, \
                 scope_conv = excluded.scope_conv, scope_agent = excluded.scope_agent, \
                 scope_kind = excluded.scope_kind, thread = excluded.thread, \
                 title = excluded.title, summary = excluded.summary, \
                 compaction_summary = excluded.compaction_summary, \
                 model_override = excluded.model_override, \
                 reasoning_effort_override = excluded.reasoning_effort_override, \
                 messages = excluded.messages, search_body = excluded.search_body, \
                 updated_at = excluded.updated_at",
            )
            .bind(session.id.as_uuid())
            .bind(session.owner.as_str())
            .bind(scope.channel.as_deref())
            .bind(scope.conv.as_deref())
            .bind(scope.agent.as_deref())
            .bind(scope.kind.as_deref())
            .bind(session.thread.as_ref().map(ThreadId::as_str))
            .bind(session.title.as_deref())
            .bind(session.summary.as_deref())
            .bind(session.compaction_summary.as_deref())
            .bind(session.model_override.as_deref())
            .bind(
                session
                    .reasoning_effort_override
                    .map(ReasoningEffort::as_str),
            )
            .bind(&messages)
            .bind(&search_body)
            .bind(session.created_at)
            .bind(session.updated_at)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn save_messages(
        &self,
        id: &SessionId,
        messages: &[Message],
        updated_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        save_messages::save_messages_impl(&self.pool, id, messages, updated_at).await
    }

    async fn get_compaction_summary(&self, id: &SessionId) -> Result<Option<String>, StoreError> {
        retry_transient("session.get_compaction_summary", || async {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT compaction_summary FROM sessions WHERE id = $1",
            )
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
        })
        .await
        .map(Option::flatten)
    }

    async fn set_compaction_summary(
        &self,
        id: &SessionId,
        summary: &str,
    ) -> Result<(), StoreError> {
        let now = Utc::now();
        let rows = retry_transient("session.set_compaction_summary", || async {
            sqlx::query(
                "UPDATE sessions SET compaction_summary = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(summary)
            .bind(now)
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected())
        })
        .await?;
        if rows == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    async fn archive_messages(
        &self,
        session_id: &SessionId,
        messages: &[Message],
        created_at: DateTime<Utc>,
    ) -> Result<ArchiveId, StoreError> {
        self.insert_archive_messages(session_id, messages, created_at)
            .await
    }

    async fn list_archives(
        &self,
        session_id: &SessionId,
        page: Page,
    ) -> Result<Vec<SessionArchiveEntry>, StoreError> {
        self.list_session_archives(session_id, page).await
    }

    async fn cleanup_old_archives(&self, days: i64) -> Result<u64, StoreError> {
        self.delete_old_archives(days).await
    }

    async fn list(&self, owner: &Owner, page: Page) -> Result<Vec<SessionInfo>, StoreError> {
        let limit = i64::try_from(page.limit)
            .map_err(|err| StoreError::invalid(format!("page.limit out of range: {err}")))?;
        let offset = i64::try_from(page.offset)
            .map_err(|err| StoreError::invalid(format!("page.offset out of range: {err}")))?;
        let rows = retry_transient("session.list", || async {
            sqlx::query_as::<_, SessionInfoRow>(
                "SELECT id, owner, thread, title, jsonb_array_length(messages) AS message_count, \
                 (summary IS NOT NULL) AS has_summary, created_at, updated_at \
                 FROM sessions WHERE owner = $1 \
                 ORDER BY updated_at DESC LIMIT $2 OFFSET $3",
            )
            .bind(owner.as_str())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn cleanup_old(&self, days: i64) -> Result<u64, StoreError> {
        let cutoff = Utc::now() - Duration::days(days);
        retry_transient("session.cleanup_old", || async {
            sqlx::query("DELETE FROM sessions WHERE updated_at < $1")
                .bind(cutoff)
                .execute(&self.pool)
                .await
                .map(|r| r.rows_affected())
        })
        .await
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SessionSearchHit>, StoreError> {
        if query.query.is_empty() {
            return list_recent(&self.pool, query).await;
        }
        search_messages(&self.pool, query).await
    }
}
