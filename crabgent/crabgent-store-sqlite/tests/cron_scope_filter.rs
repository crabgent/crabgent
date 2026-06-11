use chrono::{Duration, Utc};
use crabgent_core::{MemoryScope, Owner};
use crabgent_store::{CronJob, CronJobId, CronSchedule, CronStore, Page, Store};
use crabgent_store_sqlite::SqliteStore;
use serde_json::json;

fn job(offset_secs: i64, name: &str, scope: MemoryScope) -> CronJob {
    let now = Utc::now() + Duration::seconds(offset_secs);
    CronJob {
        id: CronJobId::new(),
        name: name.into(),
        scope,
        prompt: "say hi".into(),
        schedule: CronSchedule::every(60),
        enabled: true,
        run_once: false,
        model_override: None,
        reasoning_effort_override: None,
        pre_command: None,
        delivery_ctx: json!({}),
        last_run: None,
        next_run: now,
        created_at: now,
        claimed_at: None,
    }
}

#[tokio::test]
async fn cron_scope_filter_and() {
    let store = SqliteStore::open_in_memory()
        .await
        .expect("open sqlite store");
    let alice_im = job(
        0,
        "alice-im",
        MemoryScope::for_owner(Owner::new("alice")).with_kind("im"),
    );
    let alice_public = job(
        1,
        "alice-public",
        MemoryScope::for_owner(Owner::new("alice")).with_kind("public"),
    );
    let bob_im = job(
        2,
        "bob-im",
        MemoryScope::for_owner(Owner::new("bob")).with_kind("im"),
    );
    store.cron().create(&alice_im).await.expect("test result");
    store
        .cron()
        .create(&alice_public)
        .await
        .expect("test result");
    store.cron().create(&bob_im).await.expect("test result");

    let query = MemoryScope::for_owner(Owner::new("alice")).with_kind("im");
    let listed = store
        .cron()
        .list(&query, Page::first(10))
        .await
        .expect("test result");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, alice_im.id);
}

#[tokio::test]
async fn cron_scope_null_field_strict_match() {
    let store = SqliteStore::open_in_memory()
        .await
        .expect("open sqlite store");
    let c1 = job(0, "channel-c1", MemoryScope::default().with_channel("C1"));
    let no_channel = job(1, "no-channel", MemoryScope::default());
    store.cron().create(&c1).await.expect("test result");
    store.cron().create(&no_channel).await.expect("test result");

    let listed = store
        .cron()
        .list(&MemoryScope::default().with_channel("C1"), Page::first(10))
        .await
        .expect("test result");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, c1.id);
}
