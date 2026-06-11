//! Lifecycle operations for `SQLite` memory documents.

use chrono::{DateTime, Utc};
use crabgent_core::MemoryId;
use crabgent_store::StoreError;

use super::SqliteMemoryStore;
use crate::retry::retry_transient;

impl SqliteMemoryStore {
    pub(super) async fn archive_doc(
        &self,
        id: &MemoryId,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let id_s = id.to_string();
        let rows_affected = retry_transient("memory.archive", || async {
            sqlx::query("UPDATE memory SET archived_at = ?, updated_at = ? WHERE id = ?")
                .bind(at)
                .bind(at)
                .bind(&id_s)
                .execute(&self.pool)
                .await
                .map(|r| r.rows_affected())
        })
        .await?;
        Ok(rows_affected > 0)
    }

    pub(super) async fn unarchive_doc(&self, id: &MemoryId) -> Result<bool, StoreError> {
        let id_s = id.to_string();
        let now = Utc::now();
        let rows_affected = retry_transient("memory.unarchive", || async {
            sqlx::query("UPDATE memory SET archived_at = NULL, updated_at = ? WHERE id = ?")
                .bind(now)
                .bind(&id_s)
                .execute(&self.pool)
                .await
                .map(|r| r.rows_affected())
        })
        .await?;
        Ok(rows_affected > 0)
    }

    pub(super) async fn extend_doc_expiry(
        &self,
        id: &MemoryId,
        new_expiry: Option<DateTime<Utc>>,
    ) -> Result<bool, StoreError> {
        let id_s = id.to_string();
        let now = Utc::now();
        let rows_affected = retry_transient("memory.extend_expiry", || async {
            sqlx::query("UPDATE memory SET expires_at = ?, updated_at = ? WHERE id = ?")
                .bind(new_expiry)
                .bind(now)
                .bind(&id_s)
                .execute(&self.pool)
                .await
                .map(|r| r.rows_affected())
        })
        .await?;
        Ok(rows_affected > 0)
    }
}
