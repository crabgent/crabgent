//! Relation-edge operations for `SQLite` memory documents.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use crabgent_core::{MemoryId, MemoryScope, Owner};
use crabgent_store::error::StoreError;
use crabgent_store::ids::RelationId;
use crabgent_store::records::MemoryRelation;
use crabgent_store::relation_type::RelationType;
use crabgent_store::scope_query::{ScopeField, ScopeQuery};
use sqlx::Row;
use sqlx::sqlite::{SqlitePool, SqliteRow};

use crate::retry::retry_transient;

// ON CONFLICT DO NOTHING makes a concurrent duplicate insert (two writers past
// the SELECT-first check at once) a no-op instead of a unique-constraint error,
// matching the Postgres path. The table-level UNIQUE covers non-NULL owners;
// SQLite treats NULLs as distinct there, so a partial unique index over
// (from_id, to_id, relation_type) WHERE owner IS NULL (migration 015) closes
// the global-scope (NULL owner) gap. The bare conflict target below lets SQLite
// pick whichever index a given row violates.
const INSERT_SQL: &str = "INSERT INTO memory_relations \
    (id, from_id, to_id, relation_type, owner, channel, conv, agent, kind, created_at) \
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
    ON CONFLICT DO NOTHING";

const NATURAL_KEY_SQL: &str = "SELECT id FROM memory_relations \
    WHERE from_id = ? AND to_id = ? AND relation_type = ? AND owner IS ?";

const DELETE_SQL: &str = "DELETE FROM memory_relations \
    WHERE from_id = ? AND to_id = ? AND relation_type = ? AND owner IS ?";

const NEIGHBOR_COLS: &str =
    "id, from_id, to_id, relation_type, owner, channel, conv, agent, kind, created_at";

/// Store a relation edge. Idempotent on `(from_id, to_id, relation_type,
/// owner)`: re-storing the same edge returns the existing id. Returns
/// [`StoreError::NotFound`] when either linked document is absent or not
/// visible under the edge scope.
pub(super) async fn store_relation(
    pool: &SqlitePool,
    relation: &MemoryRelation,
) -> Result<RelationId, StoreError> {
    let from_s = relation.from_id.to_string();
    let to_s = relation.to_id.to_string();
    let type_s = relation.relation_type.as_str().to_owned();
    let owner_s = owner_string(&relation.scope);
    let channel = relation.scope.channel.clone();
    let conv = relation.scope.conv.clone();
    let agent = relation.scope.agent.clone();
    let kind = relation.scope.kind.clone();
    let id_s = relation.id.to_string();
    let created_at = relation.created_at;

    if !both_docs_visible(pool, &from_s, &to_s, &relation.scope).await? {
        return Err(StoreError::NotFound);
    }
    if let Some(existing) =
        existing_relation_id(pool, &from_s, &to_s, &type_s, owner_s.as_deref()).await?
    {
        return Ok(existing);
    }
    let inserted = retry_transient("memory.relation_store", || async {
        sqlx::query(INSERT_SQL)
            .bind(&id_s)
            .bind(&from_s)
            .bind(&to_s)
            .bind(&type_s)
            .bind(owner_s.as_deref())
            .bind(channel.as_deref())
            .bind(conv.as_deref())
            .bind(agent.as_deref())
            .bind(kind.as_deref())
            .bind(created_at)
            .execute(pool)
            .await
            .map(|r| r.rows_affected())
    })
    .await?;
    if inserted == 0 {
        // A concurrent writer won the natural key (ON CONFLICT DO NOTHING made
        // our INSERT a no-op); re-read its id.
        return match existing_relation_id(pool, &from_s, &to_s, &type_s, owner_s.as_deref()).await?
        {
            Some(existing) => Ok(existing),
            // The conflicting row vanished (deleted in a race) before the
            // re-read: our id was never persisted, so returning it would lie.
            // Mirror the Postgres path's hard error instead.
            None => Err(StoreError::backend(
                "relation insert conflicted but natural key vanished",
            )),
        };
    }
    Ok(relation.id.clone())
}

/// Delete the relation edge matching `(from_id, to_id, relation_type, owner)`.
/// Returns `true` when a row was removed.
pub(super) async fn delete_relation(
    pool: &SqlitePool,
    from_id: &MemoryId,
    to_id: &MemoryId,
    relation_type: &RelationType,
    scope: &MemoryScope,
) -> Result<bool, StoreError> {
    let from_s = from_id.to_string();
    let to_s = to_id.to_string();
    let type_s = relation_type.as_str().to_owned();
    let owner_s = owner_string(scope);

    let rows_affected = retry_transient("memory.relation_delete", || async {
        sqlx::query(DELETE_SQL)
            .bind(&from_s)
            .bind(&to_s)
            .bind(&type_s)
            .bind(owner_s.as_deref())
            .execute(pool)
            .await
            .map(|r| r.rows_affected())
    })
    .await?;
    Ok(rows_affected > 0)
}

/// Return every edge touching one of `ids` (via `from_id` or `to_id`) that is
/// visible within `scope`. Owner is widened to the agent's shared rows when
/// `scope.agent` is present, mirroring the in-memory backend.
pub(super) async fn neighbors(
    pool: &SqlitePool,
    ids: &[MemoryId],
    scope: &MemoryScope,
) -> Result<Vec<MemoryRelation>, StoreError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let id_strings: Vec<String> = ids.iter().map(MemoryId::to_string).collect();
    let mut visibility = ScopeQuery::filter(scope);
    if let Some(agent) = scope.agent.as_deref() {
        visibility.widen_owner_to_shared(agent);
    }
    let sql = build_neighbor_sql(id_strings.len(), &visibility);

    let rows = retry_transient("memory.relation_neighbors", || async {
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.clone()));
        // Bind order: the IN-clause ids appear twice (from_id then to_id),
        // then the scope filter values in canonical order.
        for value in &id_strings {
            q = q.bind(value);
        }
        for value in &id_strings {
            q = q.bind(value);
        }
        for value in visibility.equal_values() {
            q = q.bind(value.to_owned());
        }
        q.fetch_all(pool).await
    })
    .await?;
    rows.iter().map(row_to_relation).collect()
}

/// Cascade-delete every edge that references `id`. Used after a memory document
/// row is removed; the schema has no SQL foreign key, so the app does this.
pub(super) async fn cascade_delete(pool: &SqlitePool, id: &MemoryId) -> Result<(), StoreError> {
    let id_s = id.to_string();
    retry_transient("memory.relation_cascade", || async {
        sqlx::query("DELETE FROM memory_relations WHERE from_id = ? OR to_id = ?")
            .bind(&id_s)
            .bind(&id_s)
            .execute(pool)
            .await
            .map(|_| ())
    })
    .await
}

fn owner_string(scope: &MemoryScope) -> Option<String> {
    scope.owner.as_ref().map(|o| o.as_str().to_owned())
}

/// True when both endpoints exist in `memory` and are visible under the edge
/// `scope`. Visibility uses the same owner-with-shared widening as
/// [`neighbors`]: an owner-scoped caller cannot link (or probe the existence
/// of) another owner's documents. A wildcard scope field does not constrain
/// that field, so broad-scope consolidation keeps working.
async fn both_docs_visible(
    pool: &SqlitePool,
    from_s: &str,
    to_s: &str,
    scope: &MemoryScope,
) -> Result<bool, StoreError> {
    let mut visibility = ScopeQuery::filter(scope);
    if let Some(agent) = scope.agent.as_deref() {
        visibility.widen_owner_to_shared(agent);
    }
    let scope_values: Vec<String> = visibility.equal_values().map(str::to_owned).collect();
    let sql = build_visibility_sql(&visibility);

    let count = retry_transient("memory.relation_existence", || async {
        let mut q = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql.clone()))
            .bind(from_s)
            .bind(to_s);
        for value in &scope_values {
            q = q.bind(value);
        }
        q.fetch_one(pool).await
    })
    .await?;
    // Distinct ids both present and visible give 2; a self-edge with from == to
    // gives 1.
    let expected = if from_s == to_s { 1 } else { 2 };
    Ok(count == expected)
}

/// `SELECT COUNT(*) FROM memory WHERE id IN (?, ?)` plus the scope filter. The
/// two id placeholders bind first, then the scope `equal_values()` in order.
fn build_visibility_sql(visibility: &ScopeQuery<'_>) -> String {
    let mut sql = String::from("SELECT COUNT(*) FROM memory WHERE id IN (?, ?)");
    visibility.append_sql_filters(&mut sql, render_column, |s| s.push('?'));
    sql
}

async fn existing_relation_id(
    pool: &SqlitePool,
    from_s: &str,
    to_s: &str,
    type_s: &str,
    owner_s: Option<&str>,
) -> Result<Option<RelationId>, StoreError> {
    let row = retry_transient("memory.relation_lookup", || async {
        sqlx::query(NATURAL_KEY_SQL)
            .bind(from_s)
            .bind(to_s)
            .bind(type_s)
            .bind(owner_s)
            .fetch_optional(pool)
            .await
    })
    .await?;
    row.map(|r| {
        let id_s: String = r.try_get("id").map_err(StoreError::backend)?;
        RelationId::from_str(&id_s).map_err(StoreError::invalid)
    })
    .transpose()
}

fn build_neighbor_sql(id_count: usize, visibility: &ScopeQuery<'_>) -> String {
    let mut sql = String::new();
    sql.push_str("SELECT ");
    sql.push_str(NEIGHBOR_COLS);
    sql.push_str(" FROM memory_relations WHERE (from_id IN (");
    push_in_placeholders(&mut sql, id_count);
    sql.push_str(") OR to_id IN (");
    push_in_placeholders(&mut sql, id_count);
    sql.push(')');
    sql.push(')');
    visibility.append_sql_filters(&mut sql, render_column, |s| s.push('?'));
    sql
}

fn push_in_placeholders(sql: &mut String, count: usize) {
    for index in 0..count {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push('?');
    }
}

fn render_column(sql: &mut String, field: ScopeField) {
    let column = match field {
        ScopeField::Owner => "owner",
        ScopeField::Channel => "channel",
        ScopeField::Conv => "conv",
        ScopeField::Agent => "agent",
        ScopeField::Kind => "kind",
    };
    sql.push_str(column);
}

fn row_to_relation(row: &SqliteRow) -> Result<MemoryRelation, StoreError> {
    let id_s: String = row.try_get("id").map_err(StoreError::backend)?;
    let from_s: String = row.try_get("from_id").map_err(StoreError::backend)?;
    let to_s: String = row.try_get("to_id").map_err(StoreError::backend)?;
    let type_s: String = row.try_get("relation_type").map_err(StoreError::backend)?;
    let owner: Option<String> = row.try_get("owner").map_err(StoreError::backend)?;
    let channel: Option<String> = row.try_get("channel").map_err(StoreError::backend)?;
    let conv: Option<String> = row.try_get("conv").map_err(StoreError::backend)?;
    let agent: Option<String> = row.try_get("agent").map_err(StoreError::backend)?;
    let kind: Option<String> = row.try_get("kind").map_err(StoreError::backend)?;
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(StoreError::backend)?;
    Ok(MemoryRelation {
        id: RelationId::from_str(&id_s).map_err(StoreError::invalid)?,
        from_id: MemoryId::from_str(&from_s).map_err(StoreError::invalid)?,
        to_id: MemoryId::from_str(&to_s).map_err(StoreError::invalid)?,
        relation_type: RelationType::new(type_s).map_err(StoreError::invalid)?,
        scope: MemoryScope {
            owner: owner.map(Owner::new),
            channel,
            conv,
            agent,
            kind,
        },
        created_at,
    })
}

#[cfg(test)]
#[path = "relations_tests.rs"]
mod tests;
