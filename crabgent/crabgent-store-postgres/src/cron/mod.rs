//! Postgres cron sub-store.

mod claim;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::{MemoryScope, Owner, ReasoningEffort};
use crabgent_store::{
    CronJob, CronJobId, CronJobUpdate, CronSchedule, CronStore, ModelTargetDto, Page, StoreError,
    scope_query::{ScopeField, ScopeQuery},
};
use serde_json::Value;
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use std::fmt::Write;
use std::str::FromStr;
use uuid::Uuid;

use crate::retry::retry_transient;

const COLS: &str = "id, name, scope_owner, scope_channel, scope_conv, scope_agent, scope_kind, \
                    prompt, schedule, enabled, run_once, model_target, \
                    reasoning_effort_override, pre_command, delivery_ctx, last_run, next_run, \
                    created_at, claimed_at";

/// Postgres implementation of `CronStore`.
#[derive(Clone)]
pub struct PostgresCronStore {
    pub(crate) pool: PgPool,
}

impl PostgresCronStore {
    /// Create a cron sub-store from a shared pool.
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
pub(super) struct CronJobRow {
    id: Uuid,
    name: String,
    scope_owner: Option<String>,
    scope_channel: Option<String>,
    scope_conv: Option<String>,
    scope_agent: Option<String>,
    scope_kind: Option<String>,
    prompt: String,
    schedule: Json<CronSchedule>,
    enabled: bool,
    run_once: bool,
    model_target: Option<Json<ModelTargetDto>>,
    reasoning_effort_override: Option<String>,
    pre_command: Option<String>,
    delivery_ctx: Json<Value>,
    last_run: Option<DateTime<Utc>>,
    next_run: DateTime<Utc>,
    created_at: DateTime<Utc>,
    claimed_at: Option<DateTime<Utc>>,
}

impl TryFrom<CronJobRow> for CronJob {
    type Error = StoreError;

    fn try_from(row: CronJobRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: CronJobId::from_uuid(row.id),
            name: row.name,
            scope: MemoryScope {
                owner: row.scope_owner.map(Owner::new),
                channel: row.scope_channel,
                conv: row.scope_conv,
                agent: row.scope_agent,
                kind: row.scope_kind,
            },
            prompt: row.prompt,
            schedule: row.schedule.0,
            enabled: row.enabled,
            run_once: row.run_once,
            model_override: row.model_target.map(|value| value.0),
            reasoning_effort_override: row
                .reasoning_effort_override
                .map(|effort| ReasoningEffort::from_str(&effort).map_err(StoreError::invalid))
                .transpose()?,
            pre_command: row.pre_command,
            delivery_ctx: row.delivery_ctx.0,
            last_run: row.last_run,
            next_run: row.next_run,
            created_at: row.created_at,
            claimed_at: row.claimed_at,
        })
    }
}

fn build_list_sql(scope: &MemoryScope) -> String {
    let scope_query = ScopeQuery::filter(scope);
    let mut sql = format!("SELECT {COLS} FROM cron_jobs WHERE TRUE");
    let mut index = 1;
    append_scope_clause(&mut sql, &mut index, &scope_query);
    write!(
        &mut sql,
        " ORDER BY created_at LIMIT ${index} OFFSET ${}",
        index + 1
    )
    .expect("writing cron list SQL to a string cannot fail");
    sql
}

fn append_scope_clause(sql: &mut String, index: &mut i32, scope_query: &ScopeQuery<'_>) {
    scope_query.append_sql_filters(
        sql,
        |sql, field| sql.push_str(cron_scope_column(field)),
        |sql| {
            write!(sql, "${index}").expect("writing cron list SQL to a string cannot fail");
            *index += 1;
        },
    );
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

#[async_trait]
impl CronStore for PostgresCronStore {
    async fn create(&self, job: &CronJob) -> Result<(), StoreError> {
        let schedule = Json(job.schedule.clone());
        let model_target = job.model_override.clone().map(Json);
        let reasoning_effort_override = job.reasoning_effort_override.map(ReasoningEffort::as_str);
        let delivery_ctx = Json(job.delivery_ctx.clone());
        retry_transient("cron.create", || async {
            sqlx::query(
                "INSERT INTO cron_jobs (id, name, scope_owner, scope_channel, scope_conv, \
                 scope_agent, scope_kind, prompt, schedule, enabled, run_once, model_target, \
                 reasoning_effort_override, pre_command, delivery_ctx, last_run, next_run, \
                 created_at, claimed_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                 $17, $18, $19)",
            )
            .bind(job.id.as_uuid())
            .bind(&job.name)
            .bind(job.scope.owner.as_ref().map(Owner::as_str))
            .bind(job.scope.channel.as_deref())
            .bind(job.scope.conv.as_deref())
            .bind(job.scope.agent.as_deref())
            .bind(job.scope.kind.as_deref())
            .bind(&job.prompt)
            .bind(&schedule)
            .bind(job.enabled)
            .bind(job.run_once)
            .bind(model_target.as_ref())
            .bind(reasoning_effort_override)
            .bind(job.pre_command.as_deref())
            .bind(&delivery_ctx)
            .bind(job.last_run)
            .bind(job.next_run)
            .bind(job.created_at)
            .bind(job.claimed_at)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn get(&self, id: &CronJobId) -> Result<Option<CronJob>, StoreError> {
        let row = retry_transient("cron.get", || async {
            sqlx::query_as::<_, CronJobRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {COLS} FROM cron_jobs WHERE id = $1"
            )))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
        })
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    async fn list(&self, scope: &MemoryScope, page: Page) -> Result<Vec<CronJob>, StoreError> {
        let limit = i64::try_from(page.limit)
            .map_err(|err| StoreError::invalid(format!("page.limit out of range: {err}")))?;
        let offset = i64::try_from(page.offset)
            .map_err(|err| StoreError::invalid(format!("page.offset out of range: {err}")))?;
        let sql = build_list_sql(scope);
        let scope_query = ScopeQuery::filter(scope);
        let rows = retry_transient("cron.list", || async {
            let mut query = sqlx::query_as::<_, CronJobRow>(sqlx::AssertSqlSafe(sql.clone()));
            for value in scope_query.equal_values() {
                query = query.bind(value);
            }
            query.bind(limit).bind(offset).fetch_all(&self.pool).await
        })
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn update(&self, id: &CronJobId, update: &CronJobUpdate) -> Result<bool, StoreError> {
        let Some(existing) = self.get(id).await? else {
            return Ok(false);
        };
        let mut next = existing;
        update.apply_to(&mut next);

        let schedule = Json(next.schedule.clone());
        let model_target = next.model_override.clone().map(Json);
        let reasoning_effort_override = next.reasoning_effort_override.map(ReasoningEffort::as_str);
        let delivery_ctx = Json(next.delivery_ctx.clone());
        retry_transient("cron.update", || async {
            sqlx::query(
                "UPDATE cron_jobs SET name = $1, prompt = $2, schedule = $3, enabled = $4, \
                 run_once = $5, model_target = $6, reasoning_effort_override = $7, \
                 pre_command = $8, delivery_ctx = $9, next_run = $10 WHERE id = $11",
            )
            .bind(&next.name)
            .bind(&next.prompt)
            .bind(&schedule)
            .bind(next.enabled)
            .bind(next.run_once)
            .bind(model_target.as_ref())
            .bind(reasoning_effort_override)
            .bind(next.pre_command.as_deref())
            .bind(&delivery_ctx)
            .bind(next.next_run)
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await?;
        Ok(true)
    }

    async fn delete(&self, id: &CronJobId) -> Result<bool, StoreError> {
        let affected = retry_transient("cron.delete", || async {
            sqlx::query("DELETE FROM cron_jobs WHERE id = $1")
                .bind(id.as_uuid())
                .execute(&self.pool)
                .await
                .map(|result| result.rows_affected())
        })
        .await?;
        Ok(affected > 0)
    }

    async fn claim_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<CronJob>, StoreError> {
        claim::claim_due_impl(&self.pool, now, limit).await
    }

    async fn finish_claim(
        &self,
        id: &CronJobId,
        last_run: DateTime<Utc>,
        next_run: DateTime<Utc>,
        disable_run_once: bool,
    ) -> Result<(), StoreError> {
        claim::finish_claim_impl(&self.pool, id, last_run, next_run, disable_run_once).await
    }

    async fn release_claim_only(&self, id: &CronJobId) -> Result<(), StoreError> {
        claim::release_claim_only_impl(&self.pool, id).await
    }

    async fn recover_stuck(&self, timeout_secs: i64) -> Result<Vec<CronJobId>, StoreError> {
        claim::recover_stuck_impl(&self.pool, timeout_secs).await
    }
}
