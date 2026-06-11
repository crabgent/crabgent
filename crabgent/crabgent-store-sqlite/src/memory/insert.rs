//! Insert and delete operations for `SQLite` memory documents.

use crabgent_core::{MemoryId, MemoryScope};
use crabgent_store::error::StoreError;
use crabgent_store::records::MemoryDoc;
use crabgent_store::scope_query::ScopeQuery;
use sqlx::sqlite::SqlitePool;

use super::{COLS, row_to_doc};
use crate::memory::vec::encode_embedding;
use crate::retry::retry_transient;

const INSERT_SQL: &str = "INSERT INTO memory (id, owner, channel, conv, agent, kind, body, class, importance, expires_at, archived_at, embedding, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";

pub(super) async fn store_doc(pool: &SqlitePool, doc: &MemoryDoc) -> Result<MemoryId, StoreError> {
    let id = doc.id.clone();
    let id_s = id.to_string();
    let owner_s = doc.scope.owner.as_ref().map(|o| o.as_str().to_owned());
    let body_owned = doc.body.clone();
    let channel = doc.scope.channel.clone();
    let conv = doc.scope.conv.clone();
    let agent = doc.scope.agent.clone();
    let kind = doc.scope.kind.clone();
    let class = doc.class.clone();
    let importance = doc.importance;
    let expires_at = doc.expires_at;
    let archived_at = doc.archived_at;
    let embedding_blob = doc.embedding.as_deref().map(encode_embedding).transpose()?;
    let created_at = doc.created_at;
    let updated_at = doc.updated_at;
    retry_transient("memory.store", || async {
        let mut tx = pool.begin().await?;
        sqlx::query(INSERT_SQL)
            .bind(&id_s)
            .bind(owner_s.as_deref())
            .bind(channel.as_deref())
            .bind(conv.as_deref())
            .bind(agent.as_deref())
            .bind(kind.as_deref())
            .bind(&body_owned)
            .bind(class.as_deref())
            .bind(importance)
            .bind(expires_at)
            .bind(archived_at)
            .bind(embedding_blob.as_deref())
            .bind(created_at)
            .bind(updated_at)
            .execute(&mut *tx)
            .await?;
        if let Some(blob) = embedding_blob.as_deref() {
            sqlx::query("INSERT INTO memory_vec (memory_id, embedding) VALUES (?, ?)")
                .bind(&id_s)
                .bind(blob)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    })
    .await?;
    Ok(id)
}

pub(super) async fn get_doc(
    pool: &SqlitePool,
    id: &MemoryId,
) -> Result<Option<MemoryDoc>, StoreError> {
    let id_s = id.to_string();
    let row_opt = retry_transient("memory.get", || async {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM memory WHERE id = ?"
        )))
        .bind(&id_s)
        .fetch_optional(pool)
        .await
    })
    .await?;
    row_opt.as_ref().map(row_to_doc).transpose()
}

pub(super) async fn delete_doc(pool: &SqlitePool, id: &MemoryId) -> Result<bool, StoreError> {
    let id_s = id.to_string();
    let rows_affected = delete_memory_rows(pool, &id_s, "memory.delete").await?;
    Ok(rows_affected > 0)
}

pub(super) async fn delete_scoped_doc(
    pool: &SqlitePool,
    id: &MemoryId,
    scope: &MemoryScope,
) -> Result<bool, StoreError> {
    let id_s = id.to_string();
    let row_opt = get_doc(pool, id).await?;
    let Some(doc) = row_opt else {
        return Ok(false);
    };
    if !ScopeQuery::filter(scope).matches(&doc.scope) {
        return Ok(false);
    }
    let rows_affected = delete_memory_rows(pool, &id_s, "memory.delete_scoped").await?;
    Ok(rows_affected > 0)
}

async fn delete_memory_rows(
    pool: &SqlitePool,
    id: &str,
    operation: &'static str,
) -> Result<u64, StoreError> {
    retry_transient(operation, || async {
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM memory_vec WHERE memory_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let rows_affected = sqlx::query("DELETE FROM memory WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map(|r| r.rows_affected())?;
        tx.commit().await?;
        Ok::<u64, sqlx::Error>(rows_affected)
    })
    .await
}
