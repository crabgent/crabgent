//! Atomic cron claim helpers.

use chrono::{DateTime, Duration, Utc};
use crabgent_store::{CronJob, CronJobId, StoreError};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::cron::{COLS, CronJobRow};
use crate::retry::{map_sqlx_error, retry_transient};

#[derive(FromRow)]
struct IdRow {
    id: Uuid,
}

pub(super) async fn claim_due_impl(
    pool: &PgPool,
    now: DateTime<Utc>,
    limit: usize,
) -> Result<Vec<CronJob>, StoreError> {
    let limit = i64::try_from(limit)
        .map_err(|err| StoreError::invalid(format!("limit out of range: {err}")))?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| map_sqlx_error("cron.claim_due.begin", &err))?;

    let locked: Vec<IdRow> = sqlx::query_as::<_, IdRow>(
        "SELECT id FROM cron_jobs \
         WHERE enabled AND next_run <= $1 AND claimed_at IS NULL \
         ORDER BY next_run LIMIT $2 FOR UPDATE SKIP LOCKED",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await
    .map_err(|err| map_sqlx_error("cron.claim_due.select", &err))?;

    if locked.is_empty() {
        tx.commit()
            .await
            .map_err(|err| map_sqlx_error("cron.claim_due.commit", &err))?;
        return Ok(Vec::new());
    }

    let ids: Vec<Uuid> = locked.into_iter().map(|row| row.id).collect();
    let rows = sqlx::query_as::<_, CronJobRow>(sqlx::AssertSqlSafe(format!(
        "UPDATE cron_jobs SET claimed_at = $1 WHERE id = ANY($2) RETURNING {COLS}"
    )))
    .bind(now)
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(|err| map_sqlx_error("cron.claim_due.update", &err))?;

    tx.commit()
        .await
        .map_err(|err| map_sqlx_error("cron.claim_due.commit", &err))?;
    rows.into_iter().map(TryInto::try_into).collect()
}

pub(super) async fn finish_claim_impl(
    pool: &PgPool,
    id: &CronJobId,
    last_run: DateTime<Utc>,
    next_run: DateTime<Utc>,
    disable_run_once: bool,
) -> Result<(), StoreError> {
    retry_transient("cron.finish_claim", || async {
        sqlx::query(
            "UPDATE cron_jobs SET last_run = $1, next_run = $2, claimed_at = NULL, \
             enabled = CASE WHEN $3 AND run_once THEN FALSE ELSE enabled END WHERE id = $4",
        )
        .bind(last_run)
        .bind(next_run)
        .bind(disable_run_once)
        .bind(id.as_uuid())
        .execute(pool)
        .await?;
        Ok(())
    })
    .await
}

pub(super) async fn release_claim_only_impl(
    pool: &PgPool,
    id: &CronJobId,
) -> Result<(), StoreError> {
    retry_transient("cron.release_claim_only", || async {
        sqlx::query("UPDATE cron_jobs SET claimed_at = NULL WHERE id = $1")
            .bind(id.as_uuid())
            .execute(pool)
            .await?;
        Ok(())
    })
    .await
}

pub(super) async fn recover_stuck_impl(
    pool: &PgPool,
    timeout_secs: i64,
) -> Result<Vec<CronJobId>, StoreError> {
    let cutoff = Utc::now() - Duration::seconds(timeout_secs);
    let rows = retry_transient("cron.recover_stuck", || async {
        sqlx::query_as::<_, IdRow>(
            "UPDATE cron_jobs SET claimed_at = NULL \
             WHERE claimed_at IS NOT NULL AND claimed_at < $1 RETURNING id",
        )
        .bind(cutoff)
        .fetch_all(pool)
        .await
    })
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| CronJobId::from_uuid(row.id))
        .collect())
}
