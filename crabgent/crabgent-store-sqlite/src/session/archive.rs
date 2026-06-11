//! Session archive helpers for the `SQLite` session store.

use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use crabgent_core::Message;
use crabgent_store::{ArchiveId, Page, SessionArchiveEntry, SessionId, StoreError};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::retry::retry_transient;

use super::SqliteSessionStore;

fn row_to_archive(row: &SqliteRow) -> Result<SessionArchiveEntry, StoreError> {
    let id_str: String = row.try_get("id").map_err(StoreError::backend)?;
    let session_id_str: String = row.try_get("session_id").map_err(StoreError::backend)?;
    let messages_json: String = row.try_get("messages").map_err(StoreError::backend)?;
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(StoreError::backend)?;
    let messages: Vec<Message> = serde_json::from_str(&messages_json)?;
    Ok(SessionArchiveEntry {
        id: ArchiveId::from_str(&id_str).map_err(StoreError::invalid)?,
        session_id: SessionId::from_str(&session_id_str).map_err(StoreError::invalid)?,
        messages,
        created_at,
    })
}

impl SqliteSessionStore {
    pub(crate) async fn insert_archive_messages(
        &self,
        session_id: &SessionId,
        messages: &[Message],
        created_at: DateTime<Utc>,
    ) -> Result<ArchiveId, StoreError> {
        let session_id_s = session_id.to_string();
        let archive_id = ArchiveId::new();
        let archive_id_s = archive_id.to_string();
        let messages_json = serde_json::to_string(messages)?;
        retry_transient("session.archive_messages", || async {
            let mut tx = self.pool.begin().await?;
            let exists = sqlx::query_scalar::<_, i64>("SELECT 1 FROM sessions WHERE id = ?")
                .bind(&session_id_s)
                .fetch_optional(&mut *tx)
                .await?;
            if exists.is_none() {
                tx.rollback().await?;
                return Ok(false);
            }
            sqlx::query(
                "INSERT INTO session_archives (id, session_id, messages, created_at) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&archive_id_s)
            .bind(&session_id_s)
            .bind(&messages_json)
            .bind(created_at)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(true)
        })
        .await
        .and_then(|inserted| {
            if inserted {
                Ok(archive_id)
            } else {
                Err(StoreError::NotFound)
            }
        })
    }

    pub(crate) async fn list_session_archives(
        &self,
        session_id: &SessionId,
        page: Page,
    ) -> Result<Vec<SessionArchiveEntry>, StoreError> {
        let session_id_s = session_id.to_string();
        let limit = i64::try_from(page.limit).unwrap_or(i64::MAX);
        let offset = i64::try_from(page.offset)
            .map_err(|_err| StoreError::invalid("page.offset out of range"))?;
        let rows = retry_transient("session.list_archives", || async {
            sqlx::query(
                "SELECT id, session_id, messages, created_at FROM session_archives \
                 WHERE session_id = ? ORDER BY created_at DESC LIMIT ? OFFSET ?",
            )
            .bind(&session_id_s)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.iter().map(row_to_archive).collect()
    }

    pub(crate) async fn delete_old_archives(&self, days: i64) -> Result<u64, StoreError> {
        let cutoff = Utc::now() - Duration::days(days);
        retry_transient("session.cleanup_old_archives", || async {
            sqlx::query("DELETE FROM session_archives WHERE created_at < ?")
                .bind(cutoff)
                .execute(&self.pool)
                .await
                .map(|result| result.rows_affected())
        })
        .await
    }
}
