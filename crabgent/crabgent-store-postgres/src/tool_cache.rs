//! Postgres tool-cache sub-store.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_store::{SessionId, StoreError, ToolCacheEntry, ToolCacheStore};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::retry::retry_transient;

const COLS: &str = "id, session_id, tool_name, content, preview, created_at, expires_at";

/// Postgres implementation of `ToolCacheStore`.
#[derive(Clone)]
pub struct PostgresToolCacheStore {
    pub(crate) pool: PgPool,
}

impl PostgresToolCacheStore {
    /// Create a tool-cache sub-store from a shared pool.
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
struct ToolCacheRow {
    id: String,
    session_id: Uuid,
    tool_name: String,
    content: String,
    preview: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl From<ToolCacheRow> for ToolCacheEntry {
    fn from(row: ToolCacheRow) -> Self {
        Self {
            id: row.id,
            session_id: SessionId::from_uuid(row.session_id),
            tool_name: row.tool_name,
            content: row.content,
            preview: row.preview,
            created_at: row.created_at,
            expires_at: row.expires_at,
        }
    }
}

#[async_trait]
impl ToolCacheStore for PostgresToolCacheStore {
    /// Insert is idempotent: if an entry with the same `(id, session_id)`
    /// exists, no fields are updated and no error is returned. TTL refresh is
    /// not performed via insert. Use a separate operation if TTL needs updating.
    async fn insert(&self, entry: &ToolCacheEntry) -> Result<(), StoreError> {
        retry_transient("tool_cache.insert", || async {
            sqlx::query(
                "INSERT INTO tool_cache (id, session_id, tool_name, content, preview, \
                 created_at, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT(id, session_id) DO NOTHING",
            )
            .bind(&entry.id)
            .bind(entry.session_id.as_uuid())
            .bind(&entry.tool_name)
            .bind(&entry.content)
            .bind(&entry.preview)
            .bind(entry.created_at)
            .bind(entry.expires_at)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn get(
        &self,
        id: &str,
        session_id: &SessionId,
    ) -> Result<Option<ToolCacheEntry>, StoreError> {
        let now = Utc::now();
        let row = retry_transient("tool_cache.get", || async {
            sqlx::query_as::<_, ToolCacheRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM tool_cache \
                 WHERE id = $1 AND session_id = $2 AND expires_at > $3"
            )))
            .bind(id)
            .bind(session_id.as_uuid())
            .bind(now)
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        Ok(row.map(Into::into))
    }

    async fn cleanup_expired(&self) -> Result<u64, StoreError> {
        let now = Utc::now();
        retry_transient("tool_cache.cleanup_expired", || async {
            sqlx::query("DELETE FROM tool_cache WHERE expires_at <= $1")
                .bind(now)
                .execute(&self.pool)
                .await
                .map(|result| result.rows_affected())
        })
        .await
    }
}
