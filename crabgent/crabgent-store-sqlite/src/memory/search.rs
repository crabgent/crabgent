//! Search operations for `SQLite` memory documents.

use std::fmt::Write;

use crabgent_core::SearchQuery;
use crabgent_store::error::StoreError;
use crabgent_store::memory_search::{MemorySearchPlan, RankingIntent};
use crabgent_store::records::MemoryHit;
use crabgent_store::scope_query::ScopeField;
use sqlx::sqlite::SqlitePool;

use super::row_to_hit;
use crate::fts::quote_fts_phrase;
use crate::memory::vec::encode_embedding;

const ALIASED_COLS: &str = "m.id AS id, m.owner AS owner, m.channel AS channel, m.conv AS conv, \
    m.agent AS agent, m.kind AS kind, m.body AS body, m.class AS class, m.importance AS importance, \
    m.expires_at AS expires_at, m.archived_at AS archived_at, m.embedding AS embedding, \
    m.created_at AS created_at, m.updated_at AS updated_at";

struct SearchBindings<'a> {
    fts_query: Option<String>,
    embedding_blob: Option<Vec<u8>>,
    plan: MemorySearchPlan<'a>,
}

pub(super) async fn search_query(
    pool: &SqlitePool,
    query: &SearchQuery,
) -> Result<Vec<MemoryHit>, StoreError> {
    let (sql, b) = build_search_sql(query)?;
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
    if let Some(fts) = b.fts_query {
        q = q.bind(fts);
    }
    for value in b.plan.scope.equal_values() {
        q = q.bind(value.to_owned());
    }
    if let Some(class) = b.plan.class {
        q = q.bind(class);
    }
    if let Some(expires_after) = b.plan.expires_after {
        q = q.bind(expires_after);
    }
    if let Some(s) = b.plan.since {
        q = q.bind(s);
    }
    if let Some(u) = b.plan.until {
        q = q.bind(u);
    }
    if let Some(blob) = b.embedding_blob {
        let Some(vector_k) = b.plan.vector_k else {
            return Err(StoreError::backend("vector search k missing"));
        };
        q = q.bind(blob).bind(vector_k);
    }
    q = q.bind(b.plan.limit_i64).bind(b.plan.offset_i64);
    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| StoreError::backend(format!("memory.search: {e}")))?;
    rows.iter().map(row_to_hit).collect()
}

fn build_search_sql(query: &SearchQuery) -> Result<(String, SearchBindings<'_>), StoreError> {
    let mut sql = String::new();
    let plan = MemorySearchPlan::new(query);
    let fts_query = plan.text.map(quote_fts_phrase);
    let embedding_blob = plan.embedding.map(encode_embedding).transpose()?;

    if plan.embedding.is_some() {
        push_hybrid_sql(&mut sql, &plan);
    } else {
        push_fts_sql(&mut sql, &plan);
    }

    Ok((
        sql,
        SearchBindings {
            fts_query,
            embedding_blob,
            plan,
        },
    ))
}

fn push_fts_sql(sql: &mut String, plan: &MemorySearchPlan<'_>) {
    if plan.text.is_none() {
        write!(
            sql,
            "SELECT {ALIASED_COLS}, 1.0 AS score, NULL AS cosine_similarity FROM memory m WHERE 1=1"
        )
        .expect("string write");
    } else {
        write!(
            sql,
            "SELECT {ALIASED_COLS}, -bm25(memory_fts) AS score, NULL AS cosine_similarity \
             FROM memory m \
             INNER JOIN memory_fts ON m.rowid = memory_fts.rowid \
             WHERE memory_fts MATCH ?"
        )
        .expect("string write");
    }
    push_filter_sql(sql, plan, "m.");
    match plan.ranking {
        RankingIntent::ImportanceThenCreatedAt => {
            sql.push_str(" ORDER BY COALESCE(m.importance, 0.5) DESC, m.created_at DESC");
        }
        RankingIntent::TextRelevanceThenImportanceThenCreatedAt => {
            sql.push_str(
                " ORDER BY bm25(memory_fts), COALESCE(m.importance, 0.5) DESC, m.created_at DESC",
            );
        }
        RankingIntent::HybridScoreThenImportanceThenCreatedAt => {}
    }
    sql.push_str(" LIMIT ? OFFSET ?");
}

fn push_hybrid_sql(sql: &mut String, plan: &MemorySearchPlan<'_>) {
    sql.push_str("WITH candidates AS (");
    if plan.text.is_none() {
        write!(
            sql,
            "SELECT {ALIASED_COLS}, 1.0 AS score FROM memory m WHERE 1=1"
        )
        .expect("string write");
    } else {
        write!(
            sql,
            "SELECT {ALIASED_COLS}, -bm25(memory_fts) AS score \
             FROM memory m \
             INNER JOIN memory_fts ON m.rowid = memory_fts.rowid \
             WHERE memory_fts MATCH ?"
        )
        .expect("string write");
    }
    push_filter_sql(sql, plan, "m.");
    sql.push_str(
        "), nearest AS (\
         SELECT memory_id, distance FROM memory_vec \
         WHERE embedding MATCH ? AND k = ? AND memory_id IN (SELECT id FROM candidates)) \
         SELECT c.id AS id, c.owner AS owner, c.channel AS channel, c.conv AS conv, \
         c.agent AS agent, c.kind AS kind, c.body AS body, c.class AS class, \
         c.importance AS importance, c.expires_at AS expires_at, c.archived_at AS archived_at, \
         c.embedding AS embedding, c.created_at AS created_at, c.updated_at AS updated_at, \
         c.score AS score, \
         CASE WHEN nearest.distance IS NULL THEN NULL ELSE 1.0 - nearest.distance END AS cosine_similarity \
         FROM candidates c \
         LEFT JOIN nearest ON c.id = nearest.memory_id \
         ORDER BY (COALESCE(cosine_similarity, 0.0) + score) DESC, \
         COALESCE(importance, 0.5) DESC, created_at DESC \
         LIMIT ? OFFSET ?",
    );
}

fn push_filter_sql(sql: &mut String, plan: &MemorySearchPlan<'_>, prefix: &str) {
    plan.scope.append_sql_filters(
        sql,
        |sql, field| {
            write!(sql, "{prefix}{}", scope_column(field)).expect("string write");
        },
        |sql| sql.push('?'),
    );
    if plan.class.is_some() {
        write!(sql, " AND {prefix}class = ?").expect("string write");
    }
    // SQL shape only needs the predicate-presence decision here; the
    // actual `Utc::now()` timestamp is captured once in `SearchBindings`
    // (`build_search_sql`) and bound at execution. Calling `Utc::now`
    // a second time would race the shape decision against the bound
    // value and surface an off-by-microsecond divergence.
    if plan.expires_after.is_some() {
        write!(
            sql,
            " AND ({prefix}expires_at IS NULL OR {prefix}expires_at > ?)"
        )
        .expect("string write");
    }
    if plan.filter_archived {
        write!(sql, " AND {prefix}archived_at IS NULL").expect("string write");
    }
    if plan.since.is_some() {
        write!(sql, " AND {prefix}updated_at >= ?").expect("string write");
    }
    if plan.until.is_some() {
        write!(sql, " AND {prefix}updated_at <= ?").expect("string write");
    }
}

const fn scope_column(field: ScopeField) -> &'static str {
    match field {
        ScopeField::Owner => "owner",
        ScopeField::Channel => "channel",
        ScopeField::Conv => "conv",
        ScopeField::Agent => "agent",
        ScopeField::Kind => "kind",
    }
}

#[cfg(test)]
mod tests {
    use crabgent_core::{MemoryScope, Owner, SearchQuery};

    use super::build_search_sql;

    fn scope() -> MemoryScope {
        MemoryScope::for_owner(Owner::new("alice"))
    }

    #[test]
    fn build_search_sql_shape_matches_include_expired_flag() {
        // FW-8 shape guard. This test is a static SQL/bindings
        // assertion, not a runtime clock counter; the single-clock
        // invariant is established by code review of build_search_sql
        // (lone Utc::now call) plus the SAFETY comment in
        // push_filter_sql. Here we only verify the SQL predicate and
        // SearchBindings.plan.expires_after coexist iff include_expired is
        // false, so any future refactor that puts a second Utc::now
        // back into push_filter_sql would still pass this test but
        // would be caught by the code-review rule above.
        let query = SearchQuery::new("hello").scope(scope()).limit(10);
        let (sql, bindings) =
            build_search_sql(&query).expect("build_search_sql succeeds on simple query");
        assert!(
            sql.contains("expires_at IS NULL OR"),
            "expiry predicate should be emitted when include_expired=false"
        );
        assert!(
            bindings.plan.expires_after.is_some(),
            "single Utc::now() should land in SearchBindings.plan.expires_after"
        );

        let query_include_expired = SearchQuery::new("hello")
            .scope(scope())
            .include_expired()
            .limit(10);
        let (sql, bindings) = build_search_sql(&query_include_expired)
            .expect("build_search_sql succeeds with include_expired");
        assert!(
            !sql.contains("expires_at IS NULL OR"),
            "expiry predicate should be absent when include_expired=true"
        );
        assert!(
            bindings.plan.expires_after.is_none(),
            "no clock read when include_expired=true"
        );
    }

    fn shared_scope() -> MemoryScope {
        MemoryScope::for_owner(Owner::new("alice")).with_agent("shared-agent")
    }

    #[test]
    fn build_search_sql_widens_owner_in_clause_under_include_shared() {
        let query = SearchQuery::new("hello")
            .scope(shared_scope())
            .include_shared(true)
            .limit(10);
        let (sql, _b) = build_search_sql(&query).expect("build_search_sql succeeds");
        assert!(sql.contains("owner IN (?, ?)"), "rendered: {sql}");
    }

    #[test]
    fn build_search_sql_keeps_owner_equals_without_flag() {
        let query = SearchQuery::new("hello").scope(shared_scope()).limit(10);
        let (sql, _b) = build_search_sql(&query).expect("build_search_sql succeeds");
        assert!(sql.contains("owner = ?"), "rendered: {sql}");
        assert!(!sql.contains("owner IN ("), "rendered: {sql}");
    }
}
