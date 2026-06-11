//! SQLite-backed [`SessionStore`].

mod archive;
mod find_or_create;
mod immediate_transaction;
#[cfg(any(test, debug_assertions))]
pub mod pause_after_select_miss;
mod scope_sql;

use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::Row;
use sqlx::sqlite::{SqlitePool, SqliteRow};

use crabgent_core::{MemoryScope, Message, Owner, ReasoningEffort, SearchQuery, ThreadId};
use crabgent_store::{
    ArchiveId, Page, Session, SessionArchiveEntry, SessionId, SessionInfo, SessionSearchHit,
    SessionStore, StoreError,
    scope_query::ScopeQuery,
    session_support::{normalized_session_scope, session_search_text},
};

use crate::fts::quote_fts_phrase;
use crate::retry::retry_transient;
use crate::session::immediate_transaction::find_or_create_in_immediate_tx;

pub const COLS: &str = "id, owner, scope_channel, scope_conv, scope_agent, scope_kind, \
     thread, title, summary, compaction_summary, model_override, reasoning_effort_override, messages, \
     created_at, updated_at";

#[derive(Clone)]
pub struct SqliteSessionStore {
    pool: SqlitePool,
}

impl SqliteSessionStore {
    pub(crate) const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

pub fn row_to_session(row: &SqliteRow) -> Result<Session, StoreError> {
    let get = |col: &str| {
        row.try_get::<Option<String>, _>(col)
            .map_err(StoreError::backend)
    };
    let id_str: String = row.try_get("id").map_err(StoreError::backend)?;
    let owner_str: String = row.try_get("owner").map_err(StoreError::backend)?;
    let messages_json: String = row.try_get("messages").map_err(StoreError::backend)?;
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(StoreError::backend)?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at").map_err(StoreError::backend)?;
    let messages: Vec<Message> = serde_json::from_str(&messages_json)?;
    let reasoning_effort_override = get("reasoning_effort_override")?
        .map(|effort| ReasoningEffort::from_str(&effort).map_err(StoreError::invalid))
        .transpose()?;
    Ok(Session {
        id: SessionId::from_str(&id_str).map_err(StoreError::invalid)?,
        owner: Owner::new(owner_str.as_str()),
        scope: MemoryScope {
            owner: Some(Owner::new(owner_str)),
            channel: get("scope_channel")?,
            conv: get("scope_conv")?,
            agent: get("scope_agent")?,
            kind: get("scope_kind")?,
        },
        thread: get("thread")?.map(ThreadId::new),
        title: get("title")?,
        summary: get("summary")?,
        compaction_summary: get("compaction_summary")?,
        model_override: get("model_override")?,
        reasoning_effort_override,
        messages,
        created_at,
        updated_at,
    })
}

fn row_to_info(row: &SqliteRow) -> Result<SessionInfo, StoreError> {
    let id_str: String = row.try_get("id").map_err(StoreError::backend)?;
    let owner_str: String = row.try_get("owner").map_err(StoreError::backend)?;
    let thread_str: Option<String> = row.try_get("thread").map_err(StoreError::backend)?;
    let title: Option<String> = row.try_get("title").map_err(StoreError::backend)?;
    let message_count: i64 = row.try_get("message_count").map_err(StoreError::backend)?;
    let has_summary_int: i64 = row.try_get("has_summary").map_err(StoreError::backend)?;
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(StoreError::backend)?;
    let updated_at: DateTime<Utc> = row.try_get("updated_at").map_err(StoreError::backend)?;

    Ok(SessionInfo {
        id: SessionId::from_str(&id_str).map_err(StoreError::invalid)?,
        owner: Owner::new(owner_str),
        thread: thread_str.map(ThreadId::new),
        title,
        message_count: usize::try_from(message_count).unwrap_or(0),
        has_summary: has_summary_int != 0,
        created_at,
        updated_at,
    })
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn find_or_create(
        &self,
        owner: &Owner,
        thread: Option<&ThreadId>,
        scope: &MemoryScope,
    ) -> Result<Session, StoreError> {
        #[cfg(any(test, debug_assertions))]
        pause_after_select_miss::pause_after_select_miss_if_configured(
            &self.pool, owner, thread, scope,
        )
        .await?;

        find_or_create_in_immediate_tx(&self.pool, owner, thread, scope).await
    }

    async fn load(&self, id: &SessionId) -> Result<Option<Session>, StoreError> {
        let id_s = id.to_string();
        let row_opt = retry_transient("session.load", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM sessions WHERE id = ?"
            )))
            .bind(&id_s)
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row_opt.as_ref().map(row_to_session).transpose()
    }

    async fn save(&self, session: &Session) -> Result<(), StoreError> {
        let id_s = session.id.to_string();
        let owner_s = session.owner.as_str().to_owned();
        let thread_s = session.thread.as_ref().map(|t| t.as_str().to_owned());
        let messages_json = serde_json::to_string(&session.messages)?;
        let title = session.title.clone();
        let summary = session.summary.clone();
        let compaction_summary = session.compaction_summary.clone();
        let model_override = session.model_override.clone();
        let reasoning_effort_override = session
            .reasoning_effort_override
            .map(ReasoningEffort::as_str);
        let scope = normalized_session_scope(session);
        let channel = scope.channel.clone();
        let conv = scope.conv.clone();
        let agent = scope.agent.clone();
        let kind = scope.kind.clone();
        let created_at = session.created_at;
        let updated_at = session.updated_at;
        let search_body = session_search_text(session);
        retry_transient("session.save", || async {
            let mut tx = self.pool.begin().await?;
            sqlx::query(
                "INSERT INTO sessions (id, owner, scope_channel, scope_conv, scope_agent, \
                 scope_kind, thread, title, summary, compaction_summary, model_override, \
                 reasoning_effort_override, messages, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET \
                 owner = excluded.owner, scope_channel = excluded.scope_channel, \
                 scope_conv = excluded.scope_conv, scope_agent = excluded.scope_agent, \
                 scope_kind = excluded.scope_kind, thread = excluded.thread, \
                 title = excluded.title, summary = excluded.summary, \
                 compaction_summary = excluded.compaction_summary, \
                 model_override = excluded.model_override, \
                 reasoning_effort_override = excluded.reasoning_effort_override, \
                 messages = excluded.messages, updated_at = excluded.updated_at",
            )
            .bind(&id_s)
            .bind(&owner_s)
            .bind(channel.as_deref())
            .bind(conv.as_deref())
            .bind(agent.as_deref())
            .bind(kind.as_deref())
            .bind(thread_s.as_deref())
            .bind(title.as_deref())
            .bind(summary.as_deref())
            .bind(compaction_summary.as_deref())
            .bind(model_override.as_deref())
            .bind(reasoning_effort_override)
            .bind(&messages_json)
            .bind(created_at)
            .bind(updated_at)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO session_search (session_id, owner, channel, conv, agent, kind, \
                 body, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(session_id) DO UPDATE SET \
                 owner = excluded.owner, channel = excluded.channel, conv = excluded.conv, \
                 agent = excluded.agent, kind = excluded.kind, body = excluded.body, \
                 updated_at = excluded.updated_at",
            )
            .bind(&id_s)
            .bind(&owner_s)
            .bind(channel.as_deref())
            .bind(conv.as_deref())
            .bind(agent.as_deref())
            .bind(kind.as_deref())
            .bind(&search_body)
            .bind(updated_at)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(())
        })
        .await
    }

    async fn get_compaction_summary(&self, id: &SessionId) -> Result<Option<String>, StoreError> {
        let id_s = id.to_string();
        retry_transient("session.get_compaction_summary", || async {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT compaction_summary FROM sessions WHERE id = ?",
            )
            .bind(&id_s)
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
        let id_s = id.to_string();
        let now = Utc::now();
        let rows = retry_transient("session.set_compaction_summary", || async {
            let mut tx = self.pool.begin().await?;
            let rows = sqlx::query(
                "UPDATE sessions SET compaction_summary = ?, updated_at = ? WHERE id = ?",
            )
            .bind(summary)
            .bind(now)
            .bind(&id_s)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if rows > 0 {
                sqlx::query("UPDATE session_search SET updated_at = ? WHERE session_id = ?")
                    .bind(now)
                    .bind(&id_s)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
            Ok(rows)
        })
        .await?;
        if rows == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    async fn save_messages(
        &self,
        id: &SessionId,
        messages: &[Message],
        updated_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        // Recompute the FTS body from the new message list. We load the
        // current row once (cheap, single-row lookup) so the FTS body
        // includes the latest title/summary written by other code paths
        // (e.g. `models.set_session` does not touch title, but the next
        // `save_messages` should not stomp on it either).
        let snapshot = self.load(id).await?.ok_or(StoreError::NotFound)?;
        let id_s = id.to_string();
        let messages_json = serde_json::to_string(messages)?;
        let mut for_search = snapshot;
        for_search.messages = messages.to_vec();
        let search_body = session_search_text(&for_search);
        retry_transient("session.save_messages", || async {
            let mut tx = self.pool.begin().await?;
            sqlx::query("UPDATE sessions SET messages = ?, updated_at = ? WHERE id = ?")
                .bind(&messages_json)
                .bind(updated_at)
                .bind(&id_s)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE session_search SET body = ?, updated_at = ? WHERE session_id = ?")
                .bind(&search_body)
                .bind(updated_at)
                .bind(&id_s)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
        .await
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
        let owner_s = owner.as_str().to_owned();
        let limit = i64::try_from(page.limit).unwrap_or(i64::MAX);
        let offset = i64::try_from(page.offset)
            .map_err(|_err| StoreError::invalid("page.offset out of range"))?;
        let rows = retry_transient("session.list", || async {
            sqlx::query(
                "SELECT id, owner, thread, title, \
                 json_array_length(messages) AS message_count, \
                 (CASE WHEN summary IS NULL THEN 0 ELSE 1 END) AS has_summary, \
                 created_at, updated_at \
                 FROM sessions WHERE owner = ? \
                 ORDER BY updated_at DESC LIMIT ? OFFSET ?",
            )
            .bind(&owner_s)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.iter().map(row_to_info).collect()
    }

    async fn cleanup_old(&self, days: i64) -> Result<u64, StoreError> {
        let cutoff = Utc::now() - Duration::days(days);
        let affected = retry_transient("session.cleanup_old", || async {
            sqlx::query("DELETE FROM sessions WHERE updated_at < ?")
                .bind(cutoff)
                .execute(&self.pool)
                .await
                .map(|result| result.rows_affected())
        })
        .await?;
        Ok(affected)
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SessionSearchHit>, StoreError> {
        if query.query.is_empty() {
            return list_recent(&self.pool, query).await;
        }
        let q = quote_fts_phrase(&query.query);
        let scope_plan = ScopeQuery::filter(&query.scope);
        let since = query.since;
        let until = query.until;
        let limit = i64::from(query.limit);
        let offset = i64::from(query.offset);
        let mut sql = String::from(
            "SELECT s.session_id AS session_id, \
             snippet(session_messages_fts, 0, '', '', '...', 16) AS excerpt, \
             -bm25(session_messages_fts) AS score, \
             s.updated_at AS occurred_at \
             FROM session_search s \
             INNER JOIN session_messages_fts ON s.rowid = session_messages_fts.rowid \
             WHERE session_messages_fts MATCH ?",
        );
        append_session_scope_filters(&mut sql, &scope_plan, "s.");
        sql.push_str(
            " \
             AND (? IS NULL OR s.updated_at >= ?) \
             AND (? IS NULL OR s.updated_at <= ?) \
             ORDER BY bm25(session_messages_fts), s.updated_at DESC \
             LIMIT ? OFFSET ?",
        );
        let mut db_query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(&q);
        for value in scope_plan.equal_values() {
            db_query = db_query.bind(value);
        }
        let rows = db_query
            .bind(since)
            .bind(since)
            .bind(until)
            .bind(until)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StoreError::backend(format!("session.search: {e}")))?;
        rows.iter().map(row_to_search_hit).collect()
    }
}

async fn list_recent(
    pool: &SqlitePool,
    query: &SearchQuery,
) -> Result<Vec<SessionSearchHit>, StoreError> {
    let scope_plan = ScopeQuery::filter(&query.scope);
    let since = query.since;
    let until = query.until;
    let limit = i64::from(query.limit);
    let offset = i64::from(query.offset);
    let mut sql = String::from(
        "SELECT session_id, body AS excerpt, 1.0 AS score, updated_at AS occurred_at \
         FROM session_search \
         WHERE 1=1",
    );
    append_session_scope_filters(&mut sql, &scope_plan, "");
    sql.push_str(
        " \
         AND (? IS NULL OR updated_at >= ?) \
         AND (? IS NULL OR updated_at <= ?) \
         ORDER BY updated_at DESC \
         LIMIT ? OFFSET ?",
    );
    let mut db_query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for value in scope_plan.equal_values() {
        db_query = db_query.bind(value);
    }
    let rows = db_query
        .bind(since)
        .bind(since)
        .bind(until)
        .bind(until)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .map_err(|e| StoreError::backend(format!("session.search: {e}")))?;
    rows.iter().map(row_to_search_hit).collect()
}

fn append_session_scope_filters(sql: &mut String, query: &ScopeQuery<'_>, prefix: &str) {
    scope_sql::append_search_filters(sql, query, prefix);
}

fn row_to_search_hit(row: &SqliteRow) -> Result<SessionSearchHit, StoreError> {
    const MAX_EXCERPT: usize = 200;
    let id_s: String = row.try_get("session_id").map_err(StoreError::backend)?;
    let excerpt: String = row.try_get("excerpt").map_err(StoreError::backend)?;
    let score: f64 = row.try_get("score").map_err(StoreError::backend)?;
    let occurred_at: DateTime<Utc> = row.try_get("occurred_at").map_err(StoreError::backend)?;
    Ok(SessionSearchHit {
        session_id: SessionId::from_str(&id_s).map_err(StoreError::invalid)?,
        excerpt: crabgent_core::text::truncate_chars(&excerpt, MAX_EXCERPT).to_owned(),
        score,
        occurred_at,
    })
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tx_tests;
