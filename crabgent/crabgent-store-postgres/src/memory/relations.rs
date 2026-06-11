//! Relation-edge SQL builders and row mapping for the Postgres memory store.
//!
//! The `impl MemoryStore` lives in `memory.rs`; these free functions keep the
//! SQL string construction and row reconstruction out of the trait body so the
//! file stays under the LOC cap. Scope rendering reuses the same
//! `ScopeQuery::append_sql_filters` + `equal_values()` path as memory search so
//! the relation graph and the document search share one nullable-column dialect.

use std::fmt::Write;

use crabgent_core::{MemoryId, MemoryScope, Owner};
use crabgent_store::scope_query::{ScopeField, ScopeQuery};
use crabgent_store::{MemoryRelation, RelationId, RelationType, StoreError};
use sqlx::FromRow;
use sqlx::PgPool;
use uuid::Uuid;

use crate::retry::retry_transient;

/// One persisted relation edge as read back from `memory_relations`.
#[derive(FromRow)]
struct RelationRow {
    id: Uuid,
    from_id: Uuid,
    to_id: Uuid,
    relation_type: String,
    owner: Option<String>,
    channel: Option<String>,
    conv: Option<String>,
    agent: Option<String>,
    kind: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl RelationRow {
    /// Reconstruct the domain edge. Fails with [`StoreError::Invalid`] only if
    /// a stored `relation_type` violates the [`RelationType`] invariant, which
    /// the insert path guards against.
    fn into_relation(self) -> Result<MemoryRelation, StoreError> {
        Ok(MemoryRelation {
            id: RelationId::from_uuid(self.id),
            from_id: MemoryId::from_uuid(self.from_id),
            to_id: MemoryId::from_uuid(self.to_id),
            relation_type: RelationType::new(self.relation_type).map_err(StoreError::invalid)?,
            scope: MemoryScope {
                owner: self.owner.map(crabgent_core::Owner::new),
                channel: self.channel,
                conv: self.conv,
                agent: self.agent,
                kind: self.kind,
            },
            created_at: self.created_at,
        })
    }
}

/// Column name for a scope field in `memory_relations` (same layout as
/// `memory_docs`).
const fn scope_column(field: ScopeField) -> &'static str {
    match field {
        ScopeField::Owner => "owner",
        ScopeField::Channel => "channel",
        ScopeField::Conv => "conv",
        ScopeField::Agent => "agent",
        ScopeField::Kind => "kind",
    }
}

/// `SELECT id FROM memory_relations WHERE` natural key. `$4` uses
/// `IS NOT DISTINCT FROM` so a NULL owner matches a stored NULL owner.
const SELECT_BY_NATURAL_KEY: &str = "SELECT id FROM memory_relations \
     WHERE from_id = $1 AND to_id = $2 AND relation_type = $3 \
     AND owner IS NOT DISTINCT FROM $4";

/// Insert ignoring an existing natural key; `RETURNING id` is empty on conflict.
const INSERT_ON_CONFLICT: &str = "INSERT INTO memory_relations (id, from_id, to_id, relation_type, owner, channel, conv, agent, kind, created_at) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
     ON CONFLICT DO NOTHING RETURNING id";

/// Delete by natural key; `$4` null-safe via `IS NOT DISTINCT FROM`.
const DELETE_BY_NATURAL_KEY: &str = "DELETE FROM memory_relations \
     WHERE from_id = $1 AND to_id = $2 AND relation_type = $3 \
     AND owner IS NOT DISTINCT FROM $4";

/// All selectable columns in `RelationRow` order.
const COLS: &str =
    "id, from_id, to_id, relation_type, owner, channel, conv, agent, kind, created_at";

/// Store an edge idempotently on its natural key. Returns the existing
/// [`RelationId`] when the edge is already present, otherwise the freshly
/// inserted one. Fails with [`StoreError::NotFound`] when either linked
/// document is absent or not visible under the edge scope.
pub async fn store_edge(
    pool: &PgPool,
    relation: &MemoryRelation,
) -> Result<RelationId, StoreError> {
    if !both_docs_visible(pool, &relation.from_id, &relation.to_id, &relation.scope).await? {
        return Err(StoreError::NotFound);
    }
    if let Some(existing) = natural_key_id(pool, relation).await? {
        return Ok(RelationId::from_uuid(existing));
    }
    if let Some(id) = insert_on_conflict(pool, relation).await? {
        return Ok(RelationId::from_uuid(id));
    }
    // A concurrent writer won the natural key between the SELECT and the
    // INSERT (RETURNING is empty on conflict); re-read its id.
    match natural_key_id(pool, relation).await? {
        Some(id) => Ok(RelationId::from_uuid(id)),
        None => Err(StoreError::backend(
            "relation insert conflicted but natural key vanished",
        )),
    }
}

/// Delete the edge matching the natural key `(from_id, to_id, relation_type,
/// scope.owner)`, null-safe on owner. Returns `true` when a row was removed.
pub async fn delete_edge(
    pool: &PgPool,
    from_id: &MemoryId,
    to_id: &MemoryId,
    relation_type: &RelationType,
    scope: &MemoryScope,
) -> Result<bool, StoreError> {
    let owner = scope.owner.as_ref().map(Owner::as_str);
    let rows = retry_transient("memory.relation_delete", || async {
        sqlx::query(DELETE_BY_NATURAL_KEY)
            .bind(from_id.as_uuid())
            .bind(to_id.as_uuid())
            .bind(relation_type.as_str())
            .bind(owner)
            .execute(pool)
            .await
            .map(|r| r.rows_affected())
    })
    .await?;
    Ok(rows > 0)
}

/// Edges incident to any id in `ids` (as `from_id` or `to_id`) that are visible
/// under `scope`, using the owner-with-shared widening from memory search.
pub async fn neighbors(
    pool: &PgPool,
    ids: &[MemoryId],
    scope: &MemoryScope,
) -> Result<Vec<MemoryRelation>, StoreError> {
    let id_array: Vec<Uuid> = ids.iter().map(|id| *id.as_uuid()).collect();
    let rows = retry_transient("memory.relation_neighbors", || async {
        let (sql, query) = build_neighbors_sql(scope);
        let mut q = sqlx::query_as::<_, RelationRow>(sqlx::AssertSqlSafe(sql)).bind(&id_array);
        for value in query.equal_values() {
            q = q.bind(value);
        }
        q.fetch_all(pool).await
    })
    .await?;
    rows.into_iter().map(RelationRow::into_relation).collect()
}

/// Remove every edge incident to `id` (as `from_id` or `to_id`). Cascade on
/// document delete, mirroring the in-memory `cascade_relations`; no SQL FOREIGN
/// KEY does this for us.
pub async fn cascade_incident(pool: &PgPool, id: &MemoryId) -> Result<(), StoreError> {
    retry_transient("memory.relation_cascade", || async {
        sqlx::query("DELETE FROM memory_relations WHERE from_id = $1 OR to_id = $1")
            .bind(id.as_uuid())
            .execute(pool)
            .await
            .map(|_| ())
    })
    .await
}

/// True when both linked documents exist in `memory_docs` and are visible under
/// the edge `scope`. Visibility uses the same owner-with-shared widening as
/// [`neighbors`]: an owner-scoped caller cannot link (or probe the existence
/// of) another owner's documents. A wildcard scope field does not constrain
/// that field, so broad-scope consolidation keeps working.
async fn both_docs_visible(
    pool: &PgPool,
    from_id: &MemoryId,
    to_id: &MemoryId,
    scope: &MemoryScope,
) -> Result<bool, StoreError> {
    let count: i64 = retry_transient("memory.relation_doc_check", || async {
        let (sql, query) = build_visibility_sql(scope);
        let mut q = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(from_id.as_uuid())
            .bind(to_id.as_uuid());
        for value in query.equal_values() {
            q = q.bind(value);
        }
        q.fetch_one(pool).await
    })
    .await?;
    // A self-edge (from_id == to_id) counts once; both endpoints present and
    // visible means count == 2 for distinct ids, count == 1 for a self-edge.
    let expected = if from_id == to_id { 1 } else { 2 };
    Ok(count == expected)
}

/// Build the visibility count query and its scope `ScopeQuery`. `$1` and `$2`
/// bind the two endpoint ids; scope predicates take `$3..` and bind via
/// `equal_values()` in predicate order.
fn build_visibility_sql(scope: &MemoryScope) -> (String, ScopeQuery<'_>) {
    let mut query = ScopeQuery::filter(scope);
    if let Some(agent) = scope.agent.as_deref() {
        query.widen_owner_to_shared(agent);
    }
    let mut sql = String::from("SELECT count(*) FROM memory_docs WHERE (id = $1 OR id = $2)");
    let mut index: i32 = 3;
    query.append_sql_filters(
        &mut sql,
        |sql, field| sql.push_str(scope_column(field)),
        |sql| {
            write!(sql, "${index}").expect("writing relation SQL to a string cannot fail");
            index += 1;
        },
    );
    (sql, query)
}

/// Look up an existing edge id by natural key, null-safe on `owner`.
async fn natural_key_id(
    pool: &PgPool,
    relation: &MemoryRelation,
) -> Result<Option<Uuid>, StoreError> {
    let owner = relation.scope.owner.as_ref().map(Owner::as_str);
    retry_transient("memory.relation_lookup", || async {
        sqlx::query_scalar(SELECT_BY_NATURAL_KEY)
            .bind(relation.from_id.as_uuid())
            .bind(relation.to_id.as_uuid())
            .bind(relation.relation_type.as_str())
            .bind(owner)
            .fetch_optional(pool)
            .await
    })
    .await
}

/// Insert the edge, ignoring a natural-key conflict. Returns the new id or
/// `None` when a concurrent writer already owns the natural key.
async fn insert_on_conflict(
    pool: &PgPool,
    relation: &MemoryRelation,
) -> Result<Option<Uuid>, StoreError> {
    let owner = relation.scope.owner.as_ref().map(Owner::as_str);
    let channel = relation.scope.channel.as_deref();
    let conv = relation.scope.conv.as_deref();
    let agent = relation.scope.agent.as_deref();
    let kind = relation.scope.kind.as_deref();
    retry_transient("memory.relation_insert", || async {
        sqlx::query_scalar(INSERT_ON_CONFLICT)
            .bind(relation.id.as_uuid())
            .bind(relation.from_id.as_uuid())
            .bind(relation.to_id.as_uuid())
            .bind(relation.relation_type.as_str())
            .bind(owner)
            .bind(channel)
            .bind(conv)
            .bind(agent)
            .bind(kind)
            .bind(relation.created_at)
            .fetch_optional(pool)
            .await
    })
    .await
}

/// Build the neighbor query and its scope `ScopeQuery`.
///
/// The id array binds to `$1`, then scope predicates take `$2..`. The widened
/// owner-with-shared filter is rendered through `append_sql_filters`; bind its
/// `equal_values()` after the id array, in predicate order.
fn build_neighbors_sql(scope: &MemoryScope) -> (String, ScopeQuery<'_>) {
    let mut query = ScopeQuery::filter(scope);
    if let Some(agent) = scope.agent.as_deref() {
        query.widen_owner_to_shared(agent);
    }
    let mut sql =
        format!("SELECT {COLS} FROM memory_relations WHERE (from_id = ANY($1) OR to_id = ANY($1))");
    let mut index: i32 = 2;
    query.append_sql_filters(
        &mut sql,
        |sql, field| sql.push_str(scope_column(field)),
        |sql| {
            write!(sql, "${index}").expect("writing relation SQL to a string cannot fail");
            index += 1;
        },
    );
    (sql, query)
}
