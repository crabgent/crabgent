//! Session archive helpers for the Postgres session store.

use chrono::{DateTime, Duration, Utc};
use crabgent_core::Message;
use crabgent_store::{ArchiveId, Page, SessionArchiveEntry, SessionId, StoreError};
use sqlx::FromRow;
use sqlx::types::Json;
use uuid::Uuid;

use crate::retry::retry_transient;

use super::PostgresSessionStore;

#[derive(FromRow)]
struct ArchiveRow {
    id: Uuid,
    session_id: Uuid,
    messages: Json<Vec<Message>>,
    created_at: DateTime<Utc>,
}

impl From<ArchiveRow> for SessionArchiveEntry {
    fn from(row: ArchiveRow) -> Self {
        Self {
            id: ArchiveId::from_uuid(row.id),
            session_id: SessionId::from_uuid(row.session_id),
            messages: row.messages.0,
            created_at: row.created_at,
        }
    }
}

impl PostgresSessionStore {
    pub(crate) async fn insert_archive_messages(
        &self,
        session_id: &SessionId,
        messages: &[Message],
        created_at: DateTime<Utc>,
    ) -> Result<ArchiveId, StoreError> {
        let archive_id = ArchiveId::new();
        let messages = Json(messages.to_vec());
        let rows = retry_transient("session.archive_messages", || async {
            let mut tx = self.pool.begin().await?;
            let exists = sqlx::query("SELECT 1 FROM sessions WHERE id = $1")
                .bind(session_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?;
            if exists.is_none() {
                tx.rollback().await?;
                return Ok(0);
            }
            sqlx::query(
                "INSERT INTO session_archives (id, session_id, messages, created_at) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(archive_id.as_uuid())
            .bind(session_id.as_uuid())
            .bind(&messages)
            .bind(created_at)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(1)
        })
        .await?;
        if rows == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(archive_id)
    }

    pub(crate) async fn list_session_archives(
        &self,
        session_id: &SessionId,
        page: Page,
    ) -> Result<Vec<SessionArchiveEntry>, StoreError> {
        let limit = i64::try_from(page.limit)
            .map_err(|err| StoreError::invalid(format!("page.limit out of range: {err}")))?;
        let offset = i64::try_from(page.offset)
            .map_err(|err| StoreError::invalid(format!("page.offset out of range: {err}")))?;
        let rows = retry_transient("session.list_archives", || async {
            sqlx::query_as::<_, ArchiveRow>(
                "SELECT id, session_id, messages, created_at FROM session_archives \
                 WHERE session_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            )
            .bind(session_id.as_uuid())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub(crate) async fn delete_old_archives(&self, days: i64) -> Result<u64, StoreError> {
        let cutoff = Utc::now() - Duration::days(days);
        retry_transient("session.cleanup_old_archives", || async {
            sqlx::query("DELETE FROM session_archives WHERE created_at < $1")
                .bind(cutoff)
                .execute(&self.pool)
                .await
                .map(|result| result.rows_affected())
        })
        .await
    }
}
