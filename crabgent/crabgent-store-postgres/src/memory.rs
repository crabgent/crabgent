//! Postgres memory sub-store.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::{MemoryId, MemoryScope, Owner, SearchQuery};
use crabgent_store::memory_search::{MemorySearchPlan, RankingIntent};
use crabgent_store::scope_query::{ScopeField, ScopeQuery};
use crabgent_store::{
    MemoryDoc, MemoryHit, MemoryRelation, MemoryStore, RelationId, RelationType, StoreError,
};
use sqlx::FromRow;
use sqlx::PgPool;
use std::fmt::Write;
use uuid::Uuid;

use crate::fts::normalize_websearch_query;
use crate::retry::retry_transient;

#[path = "memory/relations.rs"]
mod relations;

const COLS: &str = "id, owner, channel, conv, agent, kind, body, class, importance, expires_at, archived_at, embedding, created_at, updated_at";

/// Postgres implementation of `MemoryStore`.
#[derive(Clone)]
pub struct PostgresMemoryStore {
    pub(crate) pool: PgPool,
}

impl PostgresMemoryStore {
    /// Create a memory sub-store from a shared pool.
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
struct MemoryRow {
    id: Uuid,
    owner: Option<String>,
    channel: Option<String>,
    conv: Option<String>,
    agent: Option<String>,
    kind: Option<String>,
    body: String,
    class: Option<String>,
    importance: Option<f32>,
    expires_at: Option<DateTime<Utc>>,
    archived_at: Option<DateTime<Utc>>,
    embedding: Option<pgvector::Vector>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct MemoryHitRow {
    id: Uuid,
    body: String,
    score: f32,
    cosine_similarity: Option<f32>,
    created_at: DateTime<Utc>,
}

impl From<MemoryRow> for MemoryDoc {
    fn from(row: MemoryRow) -> Self {
        Self {
            id: MemoryId::from_uuid(row.id),
            scope: MemoryScope {
                owner: row.owner.map(Owner::new),
                channel: row.channel,
                conv: row.conv,
                agent: row.agent,
                kind: row.kind,
            },
            body: row.body,
            class: row.class,
            importance: row.importance,
            expires_at: row.expires_at,
            archived_at: row.archived_at,
            embedding: row.embedding.map(|vector| vector.to_vec()),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<MemoryHitRow> for MemoryHit {
    fn from(row: MemoryHitRow) -> Self {
        Self {
            id: MemoryId::from_uuid(row.id),
            body: row.body,
            score: row.score,
            cosine_similarity: row.cosine_similarity,
            created_at: row.created_at,
        }
    }
}

struct SearchBindings<'a> {
    fts_query: Option<String>,
    plan: MemorySearchPlan<'a>,
}

fn build_search_sql(query: &SearchQuery) -> (String, SearchBindings<'_>) {
    let plan = MemorySearchPlan::new(query);
    let has_text = plan.text.is_some();
    let has_embedding = plan.embedding.is_some();
    // Pin 'german' explicitly to match the generated `search_vector` column;
    // see `fts.rs` and the german FTS migration. The 1-arg form would depend on
    // `default_text_search_config`, which a connection override could break.
    let fts_score = if has_text {
        "ts_rank(search_vector, websearch_to_tsquery('german', $1))"
    } else {
        "1.0::real"
    };
    let mut index = if has_text { 2 } else { 1 };
    let cosine_expr = if has_embedding {
        let expr = format!("(1.0 - (embedding <=> ${index}))::real");
        index += 1;
        expr
    } else {
        "NULL::real".to_owned()
    };
    let mut inner = format!(
        "SELECT {COLS}, {fts_score} AS score, {cosine_expr} AS cosine_similarity \
         FROM memory_docs"
    );
    if has_text {
        inner.push_str(" WHERE search_vector @@ websearch_to_tsquery('german', $1)");
    } else {
        inner.push_str(" WHERE TRUE");
    }
    append_scope_clause(&mut inner, &mut index, &plan);
    append_class_lifecycle_clause(&mut inner, &mut index, &plan);
    append_time_clause(&mut inner, &mut index, &plan);
    // Wrap the predicate-bearing query in a subquery so `score` and
    // `cosine_similarity` become input columns of the outer SELECT.
    // Postgres allows output aliases as standalone ORDER BY keys but not
    // inside expressions, so without this wrap the previous
    // `ORDER BY (COALESCE({cosine_expr}, 0.0) + {fts_score})` form had to
    // re-evaluate the cosine distance and FTS rank a second time per row.
    let mut sql = format!("SELECT * FROM ({inner}) AS scored");
    match plan.ranking {
        RankingIntent::ImportanceThenCreatedAt => {
            sql.push_str(" ORDER BY COALESCE(importance, 0.5) DESC, created_at DESC");
        }
        RankingIntent::TextRelevanceThenImportanceThenCreatedAt => {
            sql.push_str(" ORDER BY score DESC, COALESCE(importance, 0.5) DESC, created_at DESC");
        }
        RankingIntent::HybridScoreThenImportanceThenCreatedAt => {
            sql.push_str(
                " ORDER BY (COALESCE(cosine_similarity, 0.0) + score) DESC, \
                 COALESCE(importance, 0.5) DESC, created_at DESC",
            );
        }
    }
    write!(&mut sql, " LIMIT ${index} OFFSET ${}", index + 1)
        .expect("writing search SQL to a string cannot fail");
    (
        sql,
        SearchBindings {
            fts_query: plan.text.map(normalize_websearch_query),
            plan,
        },
    )
}

fn append_scope_clause(sql: &mut String, index: &mut i32, plan: &MemorySearchPlan<'_>) {
    plan.scope.append_sql_filters(
        sql,
        |sql, field| sql.push_str(scope_column(field)),
        |sql| {
            write!(sql, "${index}").expect("writing search SQL to a string cannot fail");
            *index += 1;
        },
    );
}

fn append_class_lifecycle_clause(sql: &mut String, index: &mut i32, plan: &MemorySearchPlan<'_>) {
    if plan.class.is_some() {
        write!(sql, " AND class = ${index}").expect("writing search SQL to a string cannot fail");
        *index += 1;
    }
    if plan.expires_after.is_some() {
        write!(sql, " AND (expires_at IS NULL OR expires_at > ${index})")
            .expect("writing search SQL to a string cannot fail");
        *index += 1;
    }
    if plan.filter_archived {
        sql.push_str(" AND archived_at IS NULL");
    }
}

fn append_time_clause(sql: &mut String, index: &mut i32, plan: &MemorySearchPlan<'_>) {
    if plan.since.is_some() {
        write!(sql, " AND updated_at >= ${index}")
            .expect("writing search SQL to a string cannot fail");
        *index += 1;
    }
    if plan.until.is_some() {
        write!(sql, " AND updated_at <= ${index}")
            .expect("writing search SQL to a string cannot fail");
        *index += 1;
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

#[async_trait]
impl MemoryStore for PostgresMemoryStore {
    async fn search(&self, query: &SearchQuery) -> Result<Vec<MemoryHit>, StoreError> {
        // Build the plan and SQL once, outside the retry closure: the plan
        // captures `Utc::now()` for the expiry boundary, so recomputing it per
        // retry would shift the lifecycle cutoff between attempts.
        let (sql, b) = build_search_sql(query);
        let plan = &b.plan;
        let fts_query = b.fts_query.as_deref();
        let rows = retry_transient("memory.search", || {
            let sql = sql.clone();
            async move {
                let mut q = sqlx::query_as::<_, MemoryHitRow>(sqlx::AssertSqlSafe(sql));
                if let Some(fts_query) = fts_query {
                    q = q.bind(fts_query.to_owned());
                }
                if let Some(embedding) = plan.embedding {
                    q = q.bind(pgvector::Vector::from(embedding.to_vec()));
                }
                for value in plan.scope.equal_values() {
                    q = q.bind(value);
                }
                if let Some(class) = plan.class {
                    q = q.bind(class);
                }
                if let Some(expires_after) = plan.expires_after {
                    q = q.bind(expires_after);
                }
                if let Some(since) = plan.since {
                    q = q.bind(since);
                }
                if let Some(until) = plan.until {
                    q = q.bind(until);
                }
                q.bind(plan.limit_i64)
                    .bind(plan.offset_i64)
                    .fetch_all(&self.pool)
                    .await
            }
        })
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn store(&self, doc: &MemoryDoc) -> Result<MemoryId, StoreError> {
        let id = doc.id.clone();
        let id_uuid = *id.as_uuid();
        let owner = doc.scope.owner.as_ref().map(Owner::as_str);
        let channel = doc.scope.channel.as_deref();
        let conv = doc.scope.conv.as_deref();
        let agent = doc.scope.agent.as_deref();
        let kind = doc.scope.kind.as_deref();
        let class = doc.class.as_deref();
        let embedding = doc.embedding.clone().map(pgvector::Vector::from);
        retry_transient("memory.store", || async {
            sqlx::query(
                "INSERT INTO memory_docs (id, owner, channel, conv, agent, kind, body, class, importance, expires_at, archived_at, embedding, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
            )
            .bind(id_uuid)
            .bind(owner)
            .bind(channel)
            .bind(conv)
            .bind(agent)
            .bind(kind)
            .bind(&doc.body)
            .bind(class)
            .bind(doc.importance)
            .bind(doc.expires_at)
            .bind(doc.archived_at)
            .bind(embedding.clone())
            .bind(doc.created_at)
            .bind(doc.updated_at)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await?;
        Ok(id)
    }

    async fn get(&self, id: &MemoryId) -> Result<Option<MemoryDoc>, StoreError> {
        let row = retry_transient("memory.get", || async {
            sqlx::query_as::<_, MemoryRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM memory_docs WHERE id = $1"
            )))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        Ok(row.map(Into::into))
    }

    async fn delete(&self, id: &MemoryId) -> Result<bool, StoreError> {
        let rows = retry_transient("memory.delete", || async {
            sqlx::query("DELETE FROM memory_docs WHERE id = $1")
                .bind(id.as_uuid())
                .execute(&self.pool)
                .await
                .map(|r| r.rows_affected())
        })
        .await?;
        if rows > 0 {
            relations::cascade_incident(&self.pool, id).await?;
        }
        Ok(rows > 0)
    }

    async fn delete_scoped(&self, id: &MemoryId, scope: &MemoryScope) -> Result<bool, StoreError> {
        let Some(doc) = self.get(id).await? else {
            return Ok(false);
        };
        if !ScopeQuery::filter(scope).matches(&doc.scope) {
            return Ok(false);
        }
        self.delete(id).await
    }

    async fn archive(&self, id: &MemoryId, at: DateTime<Utc>) -> Result<bool, StoreError> {
        let rows = retry_transient("memory.archive", || async {
            sqlx::query("UPDATE memory_docs SET archived_at = $1, updated_at = $1 WHERE id = $2")
                .bind(at)
                .bind(id.as_uuid())
                .execute(&self.pool)
                .await
                .map(|r| r.rows_affected())
        })
        .await?;
        Ok(rows > 0)
    }

    async fn unarchive(&self, id: &MemoryId) -> Result<bool, StoreError> {
        let now = Utc::now();
        let rows = retry_transient("memory.unarchive", || async {
            sqlx::query("UPDATE memory_docs SET archived_at = NULL, updated_at = $1 WHERE id = $2")
                .bind(now)
                .bind(id.as_uuid())
                .execute(&self.pool)
                .await
                .map(|r| r.rows_affected())
        })
        .await?;
        Ok(rows > 0)
    }

    async fn extend_expiry(
        &self,
        id: &MemoryId,
        new_expiry: Option<DateTime<Utc>>,
    ) -> Result<bool, StoreError> {
        let now = Utc::now();
        let rows = retry_transient("memory.extend_expiry", || async {
            sqlx::query("UPDATE memory_docs SET expires_at = $1, updated_at = $2 WHERE id = $3")
                .bind(new_expiry)
                .bind(now)
                .bind(id.as_uuid())
                .execute(&self.pool)
                .await
                .map(|r| r.rows_affected())
        })
        .await?;
        Ok(rows > 0)
    }

    async fn update_body(&self, id: &MemoryId, new_body: String) -> Result<bool, StoreError> {
        self.update_body_with_embedding(id, new_body, None).await
    }

    async fn update_body_with_embedding(
        &self,
        id: &MemoryId,
        new_body: String,
        embedding: Option<Vec<f32>>,
    ) -> Result<bool, StoreError> {
        let now = Utc::now();
        let embedding = embedding.map(pgvector::Vector::from);
        let rows = retry_transient("memory.update_body", || async {
            sqlx::query(
                "UPDATE memory_docs SET body = $1, embedding = $2, updated_at = $3 WHERE id = $4",
            )
            .bind(&new_body)
            .bind(embedding.clone())
            .bind(now)
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected())
        })
        .await?;
        Ok(rows > 0)
    }

    async fn relation_store(&self, relation: &MemoryRelation) -> Result<RelationId, StoreError> {
        relations::store_edge(&self.pool, relation).await
    }

    async fn relation_delete(
        &self,
        from_id: &MemoryId,
        to_id: &MemoryId,
        relation_type: &RelationType,
        scope: &MemoryScope,
    ) -> Result<bool, StoreError> {
        relations::delete_edge(&self.pool, from_id, to_id, relation_type, scope).await
    }

    async fn relation_neighbors(
        &self,
        ids: &[MemoryId],
        scope: &MemoryScope,
    ) -> Result<Vec<MemoryRelation>, StoreError> {
        relations::neighbors(&self.pool, ids, scope).await
    }
}

#[cfg(test)]
#[path = "memory/search_sql_tests.rs"]
mod search_sql_tests;
