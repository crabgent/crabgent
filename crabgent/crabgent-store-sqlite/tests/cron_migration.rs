use chrono::Utc;
use crabgent_store::{CronJobId, CronStore, ModelTargetDto, Store};
use crabgent_store_sqlite::SqliteStore;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

async fn legacy_pool() -> sqlx::SqlitePool {
    let opts = SqliteConnectOptions::new()
        .in_memory(true)
        .shared_cache(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("open legacy pool");
    sqlx::query(
        "CREATE TABLE cron_jobs (
            id              TEXT    PRIMARY KEY,
            name            TEXT    NOT NULL,
            owner           TEXT,
            prompt          TEXT    NOT NULL,
            schedule        TEXT    NOT NULL,
            enabled         INTEGER NOT NULL DEFAULT 1,
            run_once        INTEGER NOT NULL DEFAULT 0,
            model_override  TEXT,
            pre_command     TEXT,
            delivery_ctx    TEXT    NOT NULL DEFAULT '{}',
            last_run        TEXT,
            next_run        TEXT    NOT NULL,
            created_at      TEXT    NOT NULL,
            claimed_at      TEXT
        )",
    )
    .execute(&pool)
    .await
    .expect("create legacy cron table");
    pool
}

async fn insert_legacy_job(pool: &sqlx::SqlitePool, id: &CronJobId, model_override: &str) {
    let now = Utc::now();
    let schedule = serde_json::to_string(&crabgent_store::CronSchedule::every(60))
        .expect("serialize schedule");
    let delivery = json!({}).to_string();
    sqlx::query(
        "INSERT INTO cron_jobs (
            id, name, owner, prompt, schedule, enabled, run_once, model_override,
            pre_command, delivery_ctx, last_run, next_run, created_at, claimed_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind("legacy")
    .bind("alice")
    .bind("say hi")
    .bind(schedule)
    .bind(1_i64)
    .bind(0_i64)
    .bind(model_override)
    .bind(None::<String>)
    .bind(delivery)
    .bind(None::<String>)
    .bind(now)
    .bind(now)
    .bind(None::<String>)
    .execute(pool)
    .await
    .expect("insert legacy cron row");
}

#[tokio::test]
async fn cron_model_target_migration_converts_plain_and_provider_strings() {
    let pool = legacy_pool().await;
    let plain_id = CronJobId::new();
    let provider_id = CronJobId::new();
    insert_legacy_job(&pool, &plain_id, "opus").await;
    insert_legacy_job(&pool, &provider_id, "openai/opus").await;

    let store = SqliteStore::from_pool(pool).await.expect("migrate store");
    let plain = store
        .cron()
        .get(&plain_id)
        .await
        .expect("load plain")
        .expect("plain job exists");
    let provider = store
        .cron()
        .get(&provider_id)
        .await
        .expect("load provider")
        .expect("provider job exists");

    assert_eq!(
        plain.model_override,
        Some(ModelTargetDto::Id("opus".into()))
    );
    assert_eq!(
        provider.model_override,
        Some(ModelTargetDto::Provider {
            provider: "openai".into(),
            id: "opus".into(),
        })
    );
}
