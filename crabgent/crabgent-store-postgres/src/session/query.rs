//! Session lookup and search helpers.

use std::fmt::Write;

use crabgent_core::{MemoryScope, Owner, SearchQuery, ThreadId};
use crabgent_store::{
    SessionSearchHit, StoreError,
    scope_query::{ScopeField, ScopeQuery},
    session_support::session_identity_scope,
};
use sqlx::PgPool;

use crate::fts::normalize_websearch_query;
use crate::retry::retry_transient;

use super::{COLS, SessionRow, SessionSearchRow};

pub(super) async fn list_recent(
    pool: &PgPool,
    query: &SearchQuery,
) -> Result<Vec<SessionSearchHit>, StoreError> {
    let scope_plan = ScopeQuery::filter(&query.scope);
    let sql = build_recent_sql(&scope_plan);
    fetch_search_rows(pool, "session.search_recent", sql, &scope_plan, query, None).await
}

pub(super) async fn search_messages(
    pool: &PgPool,
    query: &SearchQuery,
) -> Result<Vec<SessionSearchHit>, StoreError> {
    let scope_plan = ScopeQuery::filter(&query.scope);
    let q = normalize_websearch_query(&query.query);
    let sql = build_text_sql(&scope_plan);
    fetch_search_rows(
        pool,
        "session.search",
        sql,
        &scope_plan,
        query,
        Some(q.as_str()),
    )
    .await
}

async fn fetch_search_rows(
    pool: &PgPool,
    operation: &'static str,
    sql: String,
    scope_plan: &ScopeQuery<'_>,
    query: &SearchQuery,
    text_query: Option<&str>,
) -> Result<Vec<SessionSearchHit>, StoreError> {
    let rows = retry_transient(operation, || async {
        let mut db_query = sqlx::query_as::<_, SessionSearchRow>(sqlx::AssertSqlSafe(sql.clone()));
        if let Some(text_query) = text_query {
            db_query = db_query.bind(text_query);
        }
        for value in scope_plan.equal_values() {
            db_query = db_query.bind(value);
        }
        db_query
            .bind(query.since)
            .bind(query.until)
            .bind(i64::from(query.limit))
            .bind(i64::from(query.offset))
            .fetch_all(pool)
            .await
    })
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

fn build_recent_sql(scope_plan: &ScopeQuery<'_>) -> String {
    let mut sql = "SELECT id AS session_id, search_body AS excerpt, \
             1.0::double precision AS score, updated_at AS occurred_at \
             FROM sessions WHERE TRUE"
        .to_owned();
    let mut index = 1;
    append_scope_clause(&mut sql, &mut index, scope_plan);
    append_time_limit_clause(&mut sql, index, "updated_at DESC");
    sql
}

fn build_text_sql(scope_plan: &ScopeQuery<'_>) -> String {
    // Pin the language to 'german' explicitly: the generated `search_vector`
    // column hardcodes `to_tsvector('german', ...)`, so the query stemmer must
    // match. The 1-arg form would instead read `default_text_search_config`,
    // which a role/session/pooler override could diverge from the index.
    let mut sql = "SELECT id AS session_id, \
                 ts_headline('german', search_body, websearch_to_tsquery('german', $1)) AS excerpt, \
                 ts_rank(search_vector, websearch_to_tsquery('german', $1))::double precision AS score, \
                 updated_at AS occurred_at \
                 FROM sessions \
                 WHERE search_vector @@ websearch_to_tsquery('german', $1)"
        .to_owned();
    let mut index = 2;
    append_scope_clause(&mut sql, &mut index, scope_plan);
    append_time_limit_clause(&mut sql, index, "score DESC, updated_at DESC");
    sql
}

fn append_scope_clause(sql: &mut String, index: &mut i32, scope_plan: &ScopeQuery<'_>) {
    scope_plan.append_sql_filters(
        sql,
        |sql, field| sql.push_str(session_scope_column(field)),
        |sql| {
            write!(sql, "${index}").expect("writing session search SQL to a string cannot fail");
            *index += 1;
        },
    );
}

fn append_time_limit_clause(sql: &mut String, index: i32, order_by: &str) {
    write!(
        sql,
        " AND (${index} IS NULL OR updated_at >= ${index}) \
         AND (${} IS NULL OR updated_at <= ${}) \
         ORDER BY {order_by} LIMIT ${} OFFSET ${}",
        index + 1,
        index + 1,
        index + 2,
        index + 3
    )
    .expect("writing session search SQL to a string cannot fail");
}

const fn session_scope_column(field: ScopeField) -> &'static str {
    match field {
        ScopeField::Owner => "owner",
        ScopeField::Channel => "scope_channel",
        ScopeField::Conv => "scope_conv",
        ScopeField::Agent => "scope_agent",
        ScopeField::Kind => "scope_kind",
    }
}

pub(super) async fn select_by_scope(
    pool: &PgPool,
    owner: &Owner,
    thread: Option<&ThreadId>,
    scope: &MemoryScope,
) -> Result<Option<SessionRow>, StoreError> {
    let identity_scope = session_identity_scope(owner, scope);
    let scope_query = ScopeQuery::identity(&identity_scope);
    let mut index = 1;
    let mut sql = format!("SELECT {COLS} FROM sessions WHERE TRUE");
    append_scope_clause(&mut sql, &mut index, &scope_query);
    write!(
        &mut sql,
        " AND (${index} IS NULL AND thread IS NULL OR thread = ${index}) \
         ORDER BY updated_at DESC LIMIT 1"
    )
    .expect("writing session find SQL to a string cannot fail");
    retry_transient("session.find", || async {
        let mut query = sqlx::query_as::<_, SessionRow>(sqlx::AssertSqlSafe(sql.clone()));
        for value in scope_query.equal_values() {
            query = query.bind(value);
        }
        query
            .bind(thread.map(ThreadId::as_str))
            .fetch_optional(pool)
            .await
    })
    .await
}

pub(super) fn truncate_excerpt(value: &str) -> String {
    const MAX_EXCERPT: usize = 200;
    if value.len() <= MAX_EXCERPT {
        return value.to_owned();
    }
    value.chars().take(MAX_EXCERPT).collect()
}

#[cfg(test)]
mod tests {
    use crabgent_core::{MemoryScope, Owner};
    use crabgent_store::scope_query::ScopeQuery;

    use super::build_text_sql;

    #[test]
    fn text_search_sql_pins_german_text_search_config() {
        // Regression: the generated sessions.search_vector column hardcodes
        // 'german', so the query side must pin the same language instead of
        // relying on a connection's default_text_search_config.
        let scope = MemoryScope::for_owner(Owner::new("alice"));
        let plan = ScopeQuery::filter(&scope);
        let sql = build_text_sql(&plan);

        assert!(
            sql.contains("websearch_to_tsquery('german', $1)"),
            "FTS query must pin german, got: {sql}"
        );
        assert!(
            sql.contains("ts_headline('german', search_body"),
            "ts_headline must pin german, got: {sql}"
        );
        assert!(
            !sql.contains("websearch_to_tsquery($1)"),
            "no bare 1-arg form may remain, got: {sql}"
        );
    }
}
