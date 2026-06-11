//! SQLite-backed [`ToolCacheStore`].

use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::{SqlitePool, SqliteRow};

use crabgent_store::{SessionId, StoreError, ToolCacheEntry, ToolCacheStore};

use crate::retry::retry_transient;

const COLS: &str = "id, session_id, tool_name, content, preview, created_at, expires_at";

#[derive(Clone)]
pub struct SqliteToolCacheStore {
    pool: SqlitePool,
}

impl SqliteToolCacheStore {
    pub(crate) const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn row_to_entry(row: &SqliteRow) -> Result<ToolCacheEntry, StoreError> {
    let id: String = row.try_get("id").map_err(StoreError::backend)?;
    let session_id: String = row.try_get("session_id").map_err(StoreError::backend)?;
    let tool_name: String = row.try_get("tool_name").map_err(StoreError::backend)?;
    let content: String = row.try_get("content").map_err(StoreError::backend)?;
    let preview: String = row.try_get("preview").map_err(StoreError::backend)?;
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(StoreError::backend)?;
    let expires_at: DateTime<Utc> = row.try_get("expires_at").map_err(StoreError::backend)?;
    Ok(ToolCacheEntry {
        id,
        session_id: SessionId::from_str(&session_id).map_err(StoreError::invalid)?,
        tool_name,
        content,
        preview,
        created_at,
        expires_at,
    })
}

#[async_trait]
impl ToolCacheStore for SqliteToolCacheStore {
    async fn insert(&self, entry: &ToolCacheEntry) -> Result<(), StoreError> {
        let id = entry.id.clone();
        let session = entry.session_id.to_string();
        let tool_name = entry.tool_name.clone();
        let content = entry.content.clone();
        let preview = entry.preview.clone();
        let created_at = entry.created_at;
        let expires_at = entry.expires_at;
        retry_transient("tool_cache.insert", || async {
            sqlx::query(
                "INSERT INTO tool_cache (id, session_id, tool_name, content, preview, \
                 created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(id, session_id) DO NOTHING",
            )
            .bind(&id)
            .bind(&session)
            .bind(&tool_name)
            .bind(&content)
            .bind(&preview)
            .bind(created_at)
            .bind(expires_at)
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
        let id_s = id.to_owned();
        let session_s = session_id.to_string();
        let now = Utc::now();
        let row_opt = retry_transient("tool_cache.get", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM tool_cache \
                 WHERE id = ? AND session_id = ? AND expires_at > ?"
            )))
            .bind(&id_s)
            .bind(&session_s)
            .bind(now)
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row_opt.as_ref().map(row_to_entry).transpose()
    }

    async fn cleanup_expired(&self) -> Result<u64, StoreError> {
        let now = Utc::now();
        let affected = retry_transient("tool_cache.cleanup_expired", || async {
            sqlx::query("DELETE FROM tool_cache WHERE expires_at <= ?")
                .bind(now)
                .execute(&self.pool)
                .await
                .map(|r| r.rows_affected())
        })
        .await?;
        Ok(affected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::SqliteStore;
    use chrono::Duration;
    use crabgent_store::Store;

    async fn store() -> SqliteStore {
        SqliteStore::open_in_memory().await.expect("open store")
    }

    fn make_entry(id: &str, session: &SessionId, expires_at: DateTime<Utc>) -> ToolCacheEntry {
        ToolCacheEntry {
            id: id.to_owned(),
            session_id: session.clone(),
            tool_name: "bash".into(),
            content: format!("output for {id}"),
            preview: "output...".into(),
            created_at: Utc::now(),
            expires_at,
        }
    }

    #[tokio::test]
    async fn insert_and_get() {
        let s = store().await;
        let session = SessionId::new();
        let entry = make_entry("c1", &session, Utc::now() + Duration::hours(1));
        s.tool_cache().insert(&entry).await.expect("test result");
        let got = s
            .tool_cache()
            .get("c1", &session)
            .await
            .expect("test result")
            .expect("test result");
        assert_eq!(got.content, "output for c1");
    }

    #[tokio::test]
    async fn get_scoped_by_session() {
        let s = store().await;
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        let entry = make_entry("c2", &session_a, Utc::now() + Duration::hours(1));
        s.tool_cache().insert(&entry).await.expect("test result");
        assert!(
            s.tool_cache()
                .get("c2", &session_b)
                .await
                .expect("test result")
                .is_none()
        );
    }

    #[tokio::test]
    async fn get_expired_returns_none() {
        let s = store().await;
        let session = SessionId::new();
        let entry = make_entry("c3", &session, Utc::now() - Duration::hours(1));
        s.tool_cache().insert(&entry).await.expect("test result");
        assert!(
            s.tool_cache()
                .get("c3", &session)
                .await
                .expect("test result")
                .is_none()
        );
    }

    #[tokio::test]
    async fn insert_idempotent() {
        let s = store().await;
        let session = SessionId::new();
        let entry = make_entry("c4", &session, Utc::now() + Duration::hours(1));
        s.tool_cache().insert(&entry).await.expect("test result");
        let mut shadow = entry.clone();
        shadow.content = "different".into();
        s.tool_cache().insert(&shadow).await.expect("test result");
        let got = s
            .tool_cache()
            .get("c4", &session)
            .await
            .expect("test result")
            .expect("test result");
        assert_eq!(got.content, "output for c4");
    }

    #[tokio::test]
    async fn cleanup_expired_removes_only_expired() {
        let s = store().await;
        let session = SessionId::new();
        let past = Utc::now() - Duration::hours(1);
        let future = Utc::now() + Duration::hours(1);
        s.tool_cache()
            .insert(&make_entry("e1", &session, past))
            .await
            .expect("test result");
        s.tool_cache()
            .insert(&make_entry("e2", &session, past))
            .await
            .expect("test result");
        s.tool_cache()
            .insert(&make_entry("v1", &session, future))
            .await
            .expect("test result");
        let removed = s.tool_cache().cleanup_expired().await.expect("test result");
        assert_eq!(removed, 2);
        assert!(
            s.tool_cache()
                .get("v1", &session)
                .await
                .expect("test result")
                .is_some()
        );
    }
}
