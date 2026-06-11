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
async fn cron_scope_smoke_lists_public_kind_across_owners() {
    let store = SqliteStore::open_in_memory()
        .await
        .expect("open sqlite store");
    let alice_public = job(
        0,
        "alice-public",
        MemoryScope::for_owner(Owner::new("alice")).with_kind("public"),
    );
    let bob_public = job(
        1,
        "bob-public",
        MemoryScope::for_owner(Owner::new("bob")).with_kind("public"),
    );
    let alice_im = job(
        2,
        "alice-im",
        MemoryScope::for_owner(Owner::new("alice")).with_kind("im"),
    );
    store
        .cron()
        .create(&alice_public)
        .await
        .expect("test result");
    store.cron().create(&bob_public).await.expect("test result");
    store.cron().create(&alice_im).await.expect("test result");

    let listed = store
        .cron()
        .list(&MemoryScope::default().with_kind("public"), Page::first(10))
        .await
        .expect("test result");
    let names = listed
        .iter()
        .map(|job| job.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["alice-public", "bob-public"]);
    assert_eq!(listed[0].scope.kind.as_deref(), Some("public"));
}
