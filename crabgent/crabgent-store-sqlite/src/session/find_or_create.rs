//! Race-safe `find_or_create` helpers for the `SQLite` session sub-store.
//!
//! Extracted from `super::session` to keep the trait-impl file under the
//! 500-LOC cap and to localise the SQL that lives inside the
//! `BEGIN IMMEDIATE` transaction.

use chrono::Utc;
use crabgent_core::{MemoryScope, Owner, ReasoningEffort, ThreadId};
use crabgent_store::scope_query::ScopeQuery;
use crabgent_store::session_support::{
    new_empty_session, normalized_session_scope, session_identity_scope, session_search_text,
};
use crabgent_store::{Session, StoreError};

use crate::retry::map_sqlx_error;
use crate::session::{COLS, row_to_session};

pub(super) async fn find_or_create_inside_tx(
    handle: &mut sqlx::SqliteConnection,
    owner: &Owner,
    thread: Option<&ThreadId>,
    scope: &MemoryScope,
) -> Result<Session, StoreError> {
    let identity_scope = session_identity_scope(owner, scope);
    let scope_query = ScopeQuery::identity(&identity_scope);
    let row_opt = select_existing_session(handle, thread, &scope_query).await?;

    if let Some(row) = row_opt {
        return row_to_session(&row);
    }

    let mut session = new_empty_session(owner, thread, Utc::now());
    let mut stamped = scope.clone();
    stamped.owner = Some(owner.clone());
    session.scope = stamped;
    insert_session_with_search(&mut *handle, &session).await?;
    Ok(session)
}

async fn select_existing_session(
    handle: &mut sqlx::SqliteConnection,
    thread: Option<&ThreadId>,
    scope_query: &ScopeQuery<'_>,
) -> Result<Option<sqlx::sqlite::SqliteRow>, StoreError> {
    let mut sql = format!("SELECT {COLS} FROM sessions WHERE 1=1");
    append_scope_identity_sql(&mut sql, scope_query);
    sql.push_str(
        " AND ((? IS NULL AND thread IS NULL) OR thread = ?) \
         ORDER BY updated_at DESC LIMIT 1",
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for value in scope_query.equal_values() {
        query = query.bind(value);
    }
    let thread_s = thread.map(ThreadId::as_str);
    query
        .bind(thread_s)
        .bind(thread_s)
        .fetch_optional(&mut *handle)
        .await
        .map_err(|e| map_sqlx_error("session.find_or_create.select", &e))
}

fn append_scope_identity_sql(sql: &mut String, scope_query: &ScopeQuery<'_>) {
    super::scope_sql::append_identity_filters(sql, scope_query);
}

async fn insert_session_with_search(
    handle: &mut sqlx::SqliteConnection,
    session: &Session,
) -> Result<(), StoreError> {
    let id_s = session.id.to_string();
    let owner_s = session.owner.as_str().to_owned();
    let thread_s = session.thread.as_ref().map(|t| t.as_str().to_owned());
    let messages_json = serde_json::to_string(&session.messages)?;
    let scope = normalized_session_scope(session);
    let search_body = session_search_text(session);
    let reasoning_effort_override = session
        .reasoning_effort_override
        .map(ReasoningEffort::as_str);
    sqlx::query(
        "INSERT INTO sessions (id, owner, scope_channel, scope_conv, scope_agent, \
         scope_kind, thread, title, summary, compaction_summary, model_override, \
         reasoning_effort_override, messages, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id_s)
    .bind(&owner_s)
    .bind(scope.channel.as_deref())
    .bind(scope.conv.as_deref())
    .bind(scope.agent.as_deref())
    .bind(scope.kind.as_deref())
    .bind(thread_s.as_deref())
    .bind(session.title.as_deref())
    .bind(session.summary.as_deref())
    .bind(session.compaction_summary.as_deref())
    .bind(session.model_override.as_deref())
    .bind(reasoning_effort_override)
    .bind(&messages_json)
    .bind(session.created_at)
    .bind(session.updated_at)
    .execute(&mut *handle)
    .await
    .map_err(|e| map_sqlx_error("session.find_or_create.insert_session", &e))?;
    sqlx::query(
        "INSERT INTO session_search (session_id, owner, channel, conv, agent, kind, \
         body, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id_s)
    .bind(&owner_s)
    .bind(scope.channel.as_deref())
    .bind(scope.conv.as_deref())
    .bind(scope.agent.as_deref())
    .bind(scope.kind.as_deref())
    .bind(&search_body)
    .bind(session.updated_at)
    .execute(&mut *handle)
    .await
    .map_err(|e| map_sqlx_error("session.find_or_create.insert_search", &e))?;
    Ok(())
}
