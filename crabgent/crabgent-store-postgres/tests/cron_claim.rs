//! `CronStore` claim integration tests.
//!
//! Each test uses `postgres_test_ctx()`, which creates a fresh database in
//! container mode and `PG_TEST_DSN` mode.

use std::sync::Arc;

#[path = "../src/test_helpers.rs"]
mod test_helpers;

use chrono::{Duration, Utc};
use crabgent_core::MemoryScope;
use crabgent_store::{CronJob, CronJobId, CronSchedule, CronStore};
use crabgent_store_postgres::{PostgresStore, cron::PostgresCronStore};
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
use test_helpers::postgres_test_ctx;
use tokio::sync::Barrier;

const CLAIM_TEST_LOCK: i64 = 7_301_202_605_120_003;

async fn claim_test_guard(pool: &PgPool) -> Transaction<'_, Postgres> {
    let mut tx = pool.begin().await.expect("test result");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(CLAIM_TEST_LOCK)
        .execute(&mut *tx)
        .await
        .expect("test result");
    tx
}

fn make_job(test_name: &str, scope: MemoryScope) -> CronJob {
    let now = Utc::now();
    CronJob {
        id: CronJobId::new(),
        name: format!("job-{test_name}"),
        scope,
        prompt: "say hi".into(),
        schedule: CronSchedule::every(60),
        enabled: true,
        run_once: false,
        model_override: None,
        reasoning_effort_override: None,
        pre_command: None,
        delivery_ctx: json!({"channel": "postgres"}),
        last_run: None,
        next_run: now,
        created_at: now,
        claimed_at: None,
    }
}

#[tokio::test]
async fn cron_claim_due_picks_due_unclaimed_jobs() {
    let ctx = postgres_test_ctx().await;
    let _guard = claim_test_guard(&ctx.pool).await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let now = Utc::now();
    let mut due = make_job("claim-due", MemoryScope::global());
    due.next_run = now - Duration::seconds(1);
    let mut future = make_job("claim-future", MemoryScope::global());
    future.next_run = now + Duration::hours(1);
    store.cron_store().create(&due).await.expect("test result");
    store
        .cron_store()
        .create(&future)
        .await
        .expect("test result");

    let claimed = store
        .cron_store()
        .claim_due(now, 5)
        .await
        .expect("test result");
    let again = store
        .cron_store()
        .claim_due(now, 5)
        .await
        .expect("test result");

    assert!(claimed.iter().any(|job| job.id == due.id));
    assert!(claimed.iter().all(|job| job.id != future.id));
    assert!(claimed.iter().all(|job| job.claimed_at.is_some()));
    assert!(again.iter().all(|job| job.id != due.id));
}

#[tokio::test]
async fn cron_claim_due_skips_disabled_jobs() {
    let ctx = postgres_test_ctx().await;
    let _guard = claim_test_guard(&ctx.pool).await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let now = Utc::now();
    let mut enabled = make_job("claim-enabled", MemoryScope::global());
    enabled.next_run = now - Duration::seconds(1);
    let mut disabled = make_job("claim-disabled", MemoryScope::global());
    disabled.enabled = false;
    disabled.next_run = now - Duration::seconds(1);
    store
        .cron_store()
        .create(&enabled)
        .await
        .expect("test result");
    store
        .cron_store()
        .create(&disabled)
        .await
        .expect("test result");

    let claimed = store
        .cron_store()
        .claim_due(now, 5)
        .await
        .expect("test result");

    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, enabled.id);
    assert!(claimed[0].enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cron_claim_due_atomic_skip_locked() {
    let ctx = postgres_test_ctx().await;
    let _guard = claim_test_guard(&ctx.pool).await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let now = Utc::now();
    for idx in 0..4 {
        let mut job = make_job(&format!("atomic-{idx}"), MemoryScope::global());
        job.next_run = now - Duration::seconds(i64::from(idx + 1));
        store.cron_store().create(&job).await.expect("test result");
    }

    let pool_a = ctx.pool.clone();
    let pool_b = ctx.pool.clone();
    let barrier = Arc::new(Barrier::new(2));
    let barrier_a = Arc::clone(&barrier);
    let barrier_b = Arc::clone(&barrier);
    let first = tokio::spawn(async move {
        let cron = PostgresCronStore::new(pool_a);
        barrier_a.wait().await;
        cron.claim_due(now, 2).await
    });
    let second = tokio::spawn(async move {
        let cron = PostgresCronStore::new(pool_b);
        barrier_b.wait().await;
        cron.claim_due(now, 2).await
    });

    let first = first.await.expect("test result").expect("test result");
    let second = second.await.expect("test result").expect("test result");
    let first_ids: Vec<_> = first.iter().map(|job| job.id.clone()).collect();
    let second_ids: Vec<_> = second.iter().map(|job| job.id.clone()).collect();

    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 2);
    assert!(first_ids.iter().all(|id| !second_ids.contains(id)));
}

#[tokio::test]
async fn cron_finish_claim_clears_and_disables_run_once() {
    let ctx = postgres_test_ctx().await;
    let _guard = claim_test_guard(&ctx.pool).await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let mut job = make_job("finish-claim", MemoryScope::global());
    job.run_once = true;
    store.cron_store().create(&job).await.expect("test result");
    let now = Utc::now();
    let claimed = store
        .cron_store()
        .claim_due(now, 1)
        .await
        .expect("test result");
    let next = now + Duration::seconds(60);

    store
        .cron_store()
        .finish_claim(&claimed[0].id, now, next, true)
        .await
        .expect("test result");
    let got = store
        .cron_store()
        .get(&job.id)
        .await
        .expect("test result")
        .expect("test result");

    assert!(got.claimed_at.is_none());
    assert!(!got.enabled);
    assert_eq!(got.last_run, Some(now));
    assert_eq!(got.next_run, next);
    assert!(
        store
            .cron_store()
            .delete(&job.id)
            .await
            .expect("test result")
    );
}

#[tokio::test]
async fn cron_release_claim_only_preserves_schedule() {
    let ctx = postgres_test_ctx().await;
    let _guard = claim_test_guard(&ctx.pool).await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let mut job = make_job("release", MemoryScope::global());
    job.last_run = Some(Utc::now() - Duration::seconds(120));
    store.cron_store().create(&job).await.expect("test result");
    let claimed = store
        .cron_store()
        .claim_due(Utc::now(), 1)
        .await
        .expect("test result");

    store
        .cron_store()
        .release_claim_only(&claimed[0].id)
        .await
        .expect("test result");
    let got = store
        .cron_store()
        .get(&job.id)
        .await
        .expect("test result")
        .expect("test result");

    assert!(got.claimed_at.is_none());
    assert_eq!(got.last_run, job.last_run);
    assert_eq!(got.next_run, job.next_run);
    assert!(
        store
            .cron_store()
            .delete(&job.id)
            .await
            .expect("test result")
    );
}

#[tokio::test]
async fn cron_recover_stuck_returns_affected_ids() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let mut job = make_job("recover", MemoryScope::global());
    job.claimed_at = Some(Utc::now() - Duration::hours(1));
    store.cron_store().create(&job).await.expect("test result");

    let recovered = store
        .cron_store()
        .recover_stuck(60)
        .await
        .expect("test result");
    let got = store
        .cron_store()
        .get(&job.id)
        .await
        .expect("test result")
        .expect("test result");

    assert!(recovered.contains(&job.id));
    assert!(got.claimed_at.is_none());
}
