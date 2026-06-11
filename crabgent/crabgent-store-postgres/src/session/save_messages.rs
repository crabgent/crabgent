//! Targeted session message updates.

use chrono::{DateTime, Utc};
use crabgent_core::Message;
use crabgent_store::session_support::session_search_text;
use crabgent_store::{Session, SessionId, StoreError};
use sqlx::PgPool;
use sqlx::types::Json;

use crate::retry::retry_transient;

use super::{COLS, SessionRow};

pub(super) async fn save_messages_impl(
    pool: &PgPool,
    id: &SessionId,
    messages: &[Message],
    updated_at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let snapshot = retry_transient("session.save_messages.load", || async {
        sqlx::query_as::<_, SessionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM sessions WHERE id = $1"
        )))
        .bind(id.as_uuid())
        .fetch_optional(pool)
        .await
    })
    .await?;
    let Some(row) = snapshot else {
        return Err(StoreError::NotFound);
    };

    let mut session: Session = row.try_into()?;
    session.messages = messages.to_vec();
    let search_body = session_search_text(&session);
    let messages = Json(messages.to_vec());
    let rows = retry_transient("session.save_messages", || async {
        sqlx::query(
            "UPDATE sessions SET messages = $1, search_body = $2, updated_at = $3 WHERE id = $4",
        )
        .bind(&messages)
        .bind(&search_body)
        .bind(updated_at)
        .bind(id.as_uuid())
        .execute(pool)
        .await
        .map(|result| result.rows_affected())
    })
    .await?;

    if rows == 0 {
        return Err(StoreError::NotFound);
    }
    Ok(())
}
