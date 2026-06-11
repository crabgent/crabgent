//! SQLite-backed [`CronStore`].

use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sqlx::Row;
use sqlx::sqlite::{SqlitePool, SqliteRow};

use crabgent_core::{MemoryScope, Owner, ReasoningEffort};
use crabgent_store::{
    CronJob, CronJobId, CronJobUpdate, CronSchedule, CronStore, Page, StoreError,
    deserialize_model_target_dto,
    scope_query::{ScopeField, ScopeQuery},
    serialize_model_target_dto,
};

use crate::retry::retry_transient;

const COLS: &str = "id, name, scope_owner, scope_channel, scope_conv, scope_agent, scope_kind, \
                    prompt, schedule, enabled, run_once, model_override, \
                    reasoning_effort_override, pre_command, delivery_ctx, last_run, next_run, \
                    created_at, claimed_at";

#[derive(Clone)]
pub struct SqliteCronStore {
    pool: SqlitePool,
}

impl SqliteCronStore {
    pub(crate) const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn row_to_job(row: &SqliteRow) -> Result<CronJob, StoreError> {
    let id: String = row.try_get("id").map_err(StoreError::backend)?;
    let name: String = row.try_get("name").map_err(StoreError::backend)?;
    let scope_owner: Option<String> = row.try_get("scope_owner").map_err(StoreError::backend)?;
    let scope_channel: Option<String> =
        row.try_get("scope_channel").map_err(StoreError::backend)?;
    let scope_conv: Option<String> = row.try_get("scope_conv").map_err(StoreError::backend)?;
    let scope_agent: Option<String> = row.try_get("scope_agent").map_err(StoreError::backend)?;
    let scope_kind: Option<String> = row.try_get("scope_kind").map_err(StoreError::backend)?;
    let prompt: String = row.try_get("prompt").map_err(StoreError::backend)?;
    let schedule_json: String = row.try_get("schedule").map_err(StoreError::backend)?;
    let enabled: i64 = row.try_get("enabled").map_err(StoreError::backend)?;
    let run_once: i64 = row.try_get("run_once").map_err(StoreError::backend)?;
    let model_override: Option<String> =
        row.try_get("model_override").map_err(StoreError::backend)?;
    let reasoning_effort_override: Option<String> = row
        .try_get("reasoning_effort_override")
        .map_err(StoreError::backend)?;
    let pre_command: Option<String> = row.try_get("pre_command").map_err(StoreError::backend)?;
    let delivery_json: String = row.try_get("delivery_ctx").map_err(StoreError::backend)?;
    let last_run: Option<DateTime<Utc>> = row.try_get("last_run").map_err(StoreError::backend)?;
    let next_run: DateTime<Utc> = row.try_get("next_run").map_err(StoreError::backend)?;
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(StoreError::backend)?;
    let claimed_at: Option<DateTime<Utc>> =
        row.try_get("claimed_at").map_err(StoreError::backend)?;

    let schedule: CronSchedule = serde_json::from_str(&schedule_json)?;
    let delivery_ctx: Value = serde_json::from_str(&delivery_json)?;
    Ok(CronJob {
        id: CronJobId::from_str(&id).map_err(StoreError::invalid)?,
        name,
        scope: MemoryScope {
            owner: scope_owner.map(Owner::new),
            channel: scope_channel,
            conv: scope_conv,
            agent: scope_agent,
            kind: scope_kind,
        },
        prompt,
        schedule,
        enabled: enabled != 0,
        run_once: run_once != 0,
        model_override: model_override.as_deref().map(deserialize_model_target_dto),
        reasoning_effort_override: reasoning_effort_override
            .map(|effort| ReasoningEffort::from_str(&effort).map_err(StoreError::invalid))
            .transpose()?,
        pre_command,
        delivery_ctx,
        last_run,
        next_run,
        created_at,
        claimed_at,
    })
}

#[async_trait]
impl CronStore for SqliteCronStore {
    async fn create(&self, job: &CronJob) -> Result<(), StoreError> {
        let id_s = job.id.to_string();
        let name = job.name.clone();
        let scope_owner = job.scope.owner.as_ref().map(|o| o.as_str().to_owned());
        let scope_channel = job.scope.channel.clone();
        let scope_conv = job.scope.conv.clone();
        let scope_agent = job.scope.agent.clone();
        let scope_kind = job.scope.kind.clone();
        let prompt = job.prompt.clone();
        let schedule_json = serde_json::to_string(&job.schedule)?;
        let enabled = i64::from(job.enabled);
        let run_once = i64::from(job.run_once);
        let model_override = job.model_override.as_ref().map(serialize_model_target_dto);
        let reasoning_effort_override = job.reasoning_effort_override.map(ReasoningEffort::as_str);
        let pre_command = job.pre_command.clone();
        let delivery_json = serde_json::to_string(&job.delivery_ctx)?;
        let last_run = job.last_run;
        let next_run = job.next_run;
        let created_at = job.created_at;
        let claimed_at = job.claimed_at;
        retry_transient("cron.create", || async {
            sqlx::query(
                "INSERT INTO cron_jobs (id, name, scope_owner, scope_channel, scope_conv, \
                 scope_agent, scope_kind, prompt, schedule, enabled, run_once, model_override, \
                 reasoning_effort_override, pre_command, delivery_ctx, last_run, next_run, \
                 created_at, claimed_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id_s)
            .bind(&name)
            .bind(scope_owner.as_deref())
            .bind(scope_channel.as_deref())
            .bind(scope_conv.as_deref())
            .bind(scope_agent.as_deref())
            .bind(scope_kind.as_deref())
            .bind(&prompt)
            .bind(&schedule_json)
            .bind(enabled)
            .bind(run_once)
            .bind(model_override.as_deref())
            .bind(reasoning_effort_override)
            .bind(pre_command.as_deref())
            .bind(&delivery_json)
            .bind(last_run)
            .bind(next_run)
            .bind(created_at)
            .bind(claimed_at)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
        .map_err(|e| match e {
            StoreError::Conflict(_) => {
                StoreError::Conflict(format!("cron job already exists: {id_s}"))
            }
            other => other,
        })
    }

    async fn get(&self, id: &CronJobId) -> Result<Option<CronJob>, StoreError> {
        let id_s = id.to_string();
        let row_opt = retry_transient("cron.get", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM cron_jobs WHERE id = ?"
            )))
            .bind(&id_s)
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row_opt.as_ref().map(row_to_job).transpose()
    }

    async fn list(&self, scope: &MemoryScope, page: Page) -> Result<Vec<CronJob>, StoreError> {
        let scope_query = ScopeQuery::filter(scope);
        let limit = i64::try_from(page.limit)
            .map_err(|err| StoreError::invalid(format!("cron list limit exceeds i64: {err}")))?;
        let offset = i64::try_from(page.offset)
            .map_err(|err| StoreError::invalid(format!("cron list offset exceeds i64: {err}")))?;
        let rows = retry_transient("cron.list", || async {
            let mut sql = format!("SELECT {COLS} FROM cron_jobs WHERE 1=1");
            scope_query.append_sql_filters(
                &mut sql,
                |sql, field| sql.push_str(cron_scope_column(field)),
                |sql| sql.push('?'),
            );
            sql.push_str(" ORDER BY created_at LIMIT ? OFFSET ?");
            let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
            for value in scope_query.equal_values() {
                q = q.bind(value);
            }
            q.bind(limit).bind(offset).fetch_all(&self.pool).await
        })
        .await?;
        rows.iter().map(row_to_job).collect()
    }

    async fn update(&self, id: &CronJobId, update: &CronJobUpdate) -> Result<bool, StoreError> {
        let Some(existing) = self.get(id).await? else {
            return Ok(false);
        };
        let mut next = existing;
        update.apply_to(&mut next);
        let id_s = next.id.to_string();
        let name = next.name.clone();
        let prompt = next.prompt.clone();
        let schedule_json = serde_json::to_string(&next.schedule)?;
        let enabled = i64::from(next.enabled);
        let run_once = i64::from(next.run_once);
        let model_override = next.model_override.as_ref().map(serialize_model_target_dto);
        let reasoning_effort_override = next.reasoning_effort_override.map(ReasoningEffort::as_str);
        let pre_command = next.pre_command.clone();
        let delivery_json = serde_json::to_string(&next.delivery_ctx)?;
        let next_run = next.next_run;
        retry_transient("cron.update", || async {
            sqlx::query(
                "UPDATE cron_jobs SET name = ?, prompt = ?, schedule = ?, enabled = ?, \
                 run_once = ?, model_override = ?, reasoning_effort_override = ?, \
                 pre_command = ?, delivery_ctx = ?, next_run = ? WHERE id = ?",
            )
            .bind(&name)
            .bind(&prompt)
            .bind(&schedule_json)
            .bind(enabled)
            .bind(run_once)
            .bind(model_override.as_deref())
            .bind(reasoning_effort_override)
            .bind(pre_command.as_deref())
            .bind(&delivery_json)
            .bind(next_run)
            .bind(&id_s)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await?;
        Ok(true)
    }

    async fn delete(&self, id: &CronJobId) -> Result<bool, StoreError> {
        let id_s = id.to_string();
        let affected = retry_transient("cron.delete", || async {
            sqlx::query("DELETE FROM cron_jobs WHERE id = ?")
                .bind(&id_s)
                .execute(&self.pool)
                .await
                .map(|r| r.rows_affected())
        })
        .await?;
        Ok(affected > 0)
    }

    async fn claim_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<CronJob>, StoreError> {
        let limit_i = i64::try_from(limit)
            .map_err(|err| StoreError::invalid(format!("cron claim limit exceeds i64: {err}")))?;
        let rows = retry_transient("cron.claim_due", || async {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE cron_jobs SET claimed_at = ? \
                 WHERE id IN ( \
                    SELECT id FROM cron_jobs \
                    WHERE enabled = 1 AND claimed_at IS NULL AND next_run <= ? \
                    ORDER BY next_run LIMIT ? \
                 ) \
                 RETURNING {COLS}"
            )))
            .bind(now)
            .bind(now)
            .bind(limit_i)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.iter().map(row_to_job).collect()
    }

    async fn finish_claim(
        &self,
        id: &CronJobId,
        last_run: DateTime<Utc>,
        next_run: DateTime<Utc>,
        disable_run_once: bool,
    ) -> Result<(), StoreError> {
        let id_s = id.to_string();
        let disable = i64::from(disable_run_once);
        retry_transient("cron.finish_claim", || async {
            sqlx::query(
                "UPDATE cron_jobs SET last_run = ?, next_run = ?, claimed_at = NULL, \
                 enabled = CASE WHEN ? = 1 AND run_once = 1 THEN 0 ELSE enabled END \
                 WHERE id = ?",
            )
            .bind(last_run)
            .bind(next_run)
            .bind(disable)
            .bind(&id_s)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn release_claim_only(&self, id: &CronJobId) -> Result<(), StoreError> {
        let id_s = id.to_string();
        retry_transient("cron.release_claim_only", || async {
            sqlx::query("UPDATE cron_jobs SET claimed_at = NULL WHERE id = ?")
                .bind(&id_s)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
    }

    async fn recover_stuck(&self, timeout_secs: i64) -> Result<Vec<CronJobId>, StoreError> {
        let cutoff = Utc::now() - Duration::seconds(timeout_secs);
        let rows = retry_transient("cron.recover_stuck", || async {
            sqlx::query(
                "UPDATE cron_jobs SET claimed_at = NULL \
                 WHERE claimed_at IS NOT NULL AND claimed_at < ? RETURNING id",
            )
            .bind(cutoff)
            .fetch_all(&self.pool)
            .await
        })
        .await?;
        rows.iter()
            .map(|r| {
                let id: String = r.try_get("id").map_err(StoreError::backend)?;
                CronJobId::from_str(&id).map_err(StoreError::invalid)
            })
            .collect()
    }
}

const fn cron_scope_column(field: ScopeField) -> &'static str {
    match field {
        ScopeField::Owner => "scope_owner",
        ScopeField::Channel => "scope_channel",
        ScopeField::Conv => "scope_conv",
        ScopeField::Agent => "scope_agent",
        ScopeField::Kind => "scope_kind",
    }
}

#[cfg(test)]
mod tests;
