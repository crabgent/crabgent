//! Update operations for `SQLite` memory documents.

use chrono::Utc;
use crabgent_core::MemoryId;
use crabgent_store::StoreError;
use sqlx::SqlitePool;

use crate::memory::vec::encode_embedding;
use crate::retry::retry_transient;

pub(super) async fn update_body_query(
    pool: &SqlitePool,
    id: &MemoryId,
    new_body: String,
) -> Result<bool, StoreError> {
    update_body_with_embedding_query(pool, id, new_body, None).await
}

pub(super) async fn update_body_with_embedding_query(
    pool: &SqlitePool,
    id: &MemoryId,
    new_body: String,
    embedding: Option<Vec<f32>>,
) -> Result<bool, StoreError> {
    let id_s = id.to_string();
    let now = Utc::now();
    let embedding_blob = embedding.as_deref().map(encode_embedding).transpose()?;
    let rows_affected = retry_transient("memory.update_body", || async {
        let mut tx = pool.begin().await?;
        let rows_affected =
            sqlx::query("UPDATE memory SET body = ?, embedding = ?, updated_at = ? WHERE id = ?")
                .bind(&new_body)
                .bind(embedding_blob.as_deref())
                .bind(now)
                .bind(&id_s)
                .execute(&mut *tx)
                .await
                .map(|r| r.rows_affected())?;
        if rows_affected > 0 {
            sqlx::query("DELETE FROM memory_vec WHERE memory_id = ?")
                .bind(&id_s)
                .execute(&mut *tx)
                .await?;
            if let Some(blob) = embedding_blob.as_deref() {
                sqlx::query("INSERT INTO memory_vec (memory_id, embedding) VALUES (?, ?)")
                    .bind(&id_s)
                    .bind(blob)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        Ok::<u64, sqlx::Error>(rows_affected)
    })
    .await?;
    Ok(rows_affected > 0)
}
