//! `CronStore` integration tests.
//!
//! Each test uses `postgres_test_ctx()`, which creates a fresh database in
//! container mode and `PG_TEST_DSN` mode.

#[path = "../src/test_helpers.rs"]
mod test_helpers;

use chrono::Utc;
use crabgent_core::{MemoryScope, Owner};
use crabgent_store::{
    CronJob, CronJobId, CronJobUpdate, CronSchedule, CronStore, ModelTargetDto, Page, StoreError,
};
use crabgent_store_postgres::PostgresStore;
use serde_json::json;
use test_helpers::postgres_test_ctx;
use uuid::Uuid;

fn owner(test_name: &str) -> Owner {
    Owner::new(format!("pg-cron-{test_name}-{}", Uuid::now_v7()))
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
async fn cron_create_get_roundtrip() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let scope = MemoryScope::for_owner(owner("create-get"))
        .with_channel("postgres")
        .with_conv("cron-room")
        .with_agent("worker")
        .with_kind("public");
    let mut job = make_job("create-get", scope);
    job.model_override = Some(ModelTargetDto::Provider {
        provider: "openai".into(),
        id: "gpt-test".into(),
    });

    store.cron_store().create(&job).await.expect("test result");
    let got = store
        .cron_store()
        .get(&job.id)
        .await
        .expect("test result")
        .expect("job exists");

    assert_eq!(got.name, job.name);
    assert_eq!(got.scope, job.scope);
    assert_eq!(got.delivery_ctx["channel"], "postgres");
    assert_eq!(got.model_override, job.model_override);
}

#[tokio::test]
async fn cron_duplicate_create_conflicts() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let job = make_job("duplicate", MemoryScope::global());

    store.cron_store().create(&job).await.expect("test result");
    let err = store
        .cron_store()
        .create(&job)
        .await
        .expect_err("expected error");

    assert!(matches!(err, StoreError::Conflict(_)));
}

#[tokio::test]
async fn cron_delete_returns_true_then_false() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let job = make_job("delete", MemoryScope::global());
    store.cron_store().create(&job).await.expect("test result");

    assert!(
        store
            .cron_store()
            .delete(&job.id)
            .await
            .expect("test result")
    );
    assert!(
        !store
            .cron_store()
            .delete(&job.id)
            .await
            .expect("test result")
    );
}

#[tokio::test]
async fn cron_list_filters_by_scope_owner_or_global() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let alice_owner = owner("list-alice");
    let alice_scope = MemoryScope::for_owner(alice_owner);
    let alice = make_job("list-alice", alice_scope.clone());
    let bob = make_job("list-bob", MemoryScope::for_owner(owner("list-bob")));
    let global = make_job("list-global", MemoryScope::global());
    store
        .cron_store()
        .create(&alice)
        .await
        .expect("test result");
    store.cron_store().create(&bob).await.expect("test result");
    store
        .cron_store()
        .create(&global)
        .await
        .expect("test result");

    let alice_only = store
        .cron_store()
        .list(&alice_scope, Page::first(1000))
        .await
        .expect("test result");
    let all_jobs = store
        .cron_store()
        .list(&MemoryScope::global(), Page::first(1000))
        .await
        .expect("test result");

    assert!(alice_only.iter().any(|job| job.id == alice.id));
    assert!(!alice_only.iter().any(|job| job.id == bob.id));
    assert!(all_jobs.iter().any(|job| job.id == alice.id));
    assert!(all_jobs.iter().any(|job| job.id == bob.id));
    assert!(all_jobs.iter().any(|job| job.id == global.id));
}

#[tokio::test]
async fn cron_scope_smoke() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let public_alice = make_job(
        "scope-public-alice",
        MemoryScope::for_owner(owner("smoke-alice")).with_kind("public"),
    );
    let public_bob = make_job(
        "scope-public-bob",
        MemoryScope::for_owner(owner("smoke-bob")).with_kind("public"),
    );
    let private_alice = make_job(
        "scope-private-alice",
        MemoryScope::for_owner(owner("smoke-private")).with_kind("im"),
    );
    store
        .cron_store()
        .create(&public_alice)
        .await
        .expect("test result");
    store
        .cron_store()
        .create(&public_bob)
        .await
        .expect("test result");
    store
        .cron_store()
        .create(&private_alice)
        .await
        .expect("test result");

    let public_jobs = store
        .cron_store()
        .list(
            &MemoryScope::global().with_kind("public"),
            Page::first(1000),
        )
        .await
        .expect("test result");

    assert!(public_jobs.iter().any(|job| job.id == public_alice.id));
    assert!(public_jobs.iter().any(|job| job.id == public_bob.id));
    assert!(!public_jobs.iter().any(|job| job.id == private_alice.id));
}

#[tokio::test]
async fn cron_scope_filter_and() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let alice_owner = owner("filter-and-alice");
    let alice_im_scope = MemoryScope::for_owner(alice_owner.clone()).with_kind("im");
    let alice_im = make_job("filter-and-alice-im", alice_im_scope.clone());
    let alice_public = make_job(
        "filter-and-alice-public",
        MemoryScope::for_owner(alice_owner).with_kind("public"),
    );
    let bob_im = make_job(
        "filter-and-bob-im",
        MemoryScope::for_owner(owner("filter-and-bob")).with_kind("im"),
    );
    store
        .cron_store()
        .create(&alice_im)
        .await
        .expect("test result");
    store
        .cron_store()
        .create(&alice_public)
        .await
        .expect("test result");
    store
        .cron_store()
        .create(&bob_im)
        .await
        .expect("test result");

    let filtered = store
        .cron_store()
        .list(&alice_im_scope, Page::first(1000))
        .await
        .expect("test result");

    assert!(filtered.iter().any(|job| job.id == alice_im.id));
    assert!(!filtered.iter().any(|job| job.id == alice_public.id));
    assert!(!filtered.iter().any(|job| job.id == bob_im.id));
}

#[tokio::test]
async fn cron_scope_null_field_strict_match() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let channel_job = make_job("scope-channel", MemoryScope::global().with_channel("C1"));
    let no_channel_job = make_job("scope-no-channel", MemoryScope::global());
    store
        .cron_store()
        .create(&channel_job)
        .await
        .expect("test result");
    store
        .cron_store()
        .create(&no_channel_job)
        .await
        .expect("test result");

    let filtered = store
        .cron_store()
        .list(&MemoryScope::global().with_channel("C1"), Page::first(1000))
        .await
        .expect("test result");

    assert!(filtered.iter().any(|job| job.id == channel_job.id));
    assert!(!filtered.iter().any(|job| job.id == no_channel_job.id));
}

#[tokio::test]
async fn cron_update_applies_partial_fields() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let job = make_job("update", MemoryScope::global());
    store.cron_store().create(&job).await.expect("test result");
    let update = CronJobUpdate {
        enabled: Some(false),
        prompt: Some("new prompt".into()),
        delivery_ctx: Some(json!({"channel": "updated"})),
        ..Default::default()
    };

    let applied = store
        .cron_store()
        .update(&job.id, &update)
        .await
        .expect("test result");
    let got = store
        .cron_store()
        .get(&job.id)
        .await
        .expect("test result")
        .expect("test result");

    assert!(applied);
    assert!(!got.enabled);
    assert_eq!(got.prompt, "new prompt");
    assert_eq!(got.delivery_ctx["channel"], "updated");
}
