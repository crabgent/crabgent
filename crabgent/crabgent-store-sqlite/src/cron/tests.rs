use super::*;
use crate::backend::SqliteStore;
use crabgent_store::Store;
use serde_json::json;

async fn store() -> SqliteStore {
    SqliteStore::open_in_memory().await.expect("open store")
}

fn make_job(now: DateTime<Utc>, scope: MemoryScope) -> CronJob {
    CronJob {
        id: CronJobId::new(),
        name: "ping".into(),
        scope,
        prompt: "say hi".into(),
        schedule: CronSchedule::every(60),
        enabled: true,
        run_once: false,
        model_override: None,
        reasoning_effort_override: None,
        pre_command: None,
        delivery_ctx: json!({"channel": "matrix"}),
        last_run: None,
        next_run: now,
        created_at: now,
        claimed_at: None,
    }
}

#[tokio::test]
async fn create_get_round_trip() {
    let s = store().await;
    let job = make_job(Utc::now(), MemoryScope::for_owner(Owner::new("u")));
    s.cron().create(&job).await.expect("test result");
    let got = s
        .cron()
        .get(&job.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.name, "ping");
    assert_eq!(got.delivery_ctx["channel"], "matrix");
}

#[tokio::test]
async fn duplicate_create_conflicts() {
    let s = store().await;
    let job = make_job(Utc::now(), MemoryScope::default());
    s.cron().create(&job).await.expect("test result");
    let err = s.cron().create(&job).await.expect_err("expected error");
    assert!(matches!(err, StoreError::Conflict(_)));
}

#[tokio::test]
async fn delete_returns_true_then_false() {
    let s = store().await;
    let job = make_job(Utc::now(), MemoryScope::default());
    s.cron().create(&job).await.expect("test result");
    assert!(s.cron().delete(&job.id).await.expect("test result"));
    assert!(!s.cron().delete(&job.id).await.expect("test result"));
}

#[tokio::test]
async fn update_applies_partial_fields() {
    let s = store().await;
    let job = make_job(Utc::now(), MemoryScope::default());
    s.cron().create(&job).await.expect("test result");
    let upd = CronJobUpdate {
        enabled: Some(false),
        prompt: Some("new prompt".into()),
        ..Default::default()
    };
    let applied = s.cron().update(&job.id, &upd).await.expect("test result");
    assert!(applied);
    let got = s
        .cron()
        .get(&job.id)
        .await
        .expect("test result")
        .expect("test result");
    assert!(!got.enabled);
    assert_eq!(got.prompt, "new prompt");
}

#[tokio::test]
async fn claim_due_picks_only_due_unclaimed_jobs() {
    let s = store().await;
    let now = Utc::now();
    let due = make_job(now - Duration::seconds(1), MemoryScope::default());
    let future = CronJob {
        id: CronJobId::new(),
        next_run: now + Duration::hours(1),
        ..make_job(now, MemoryScope::default())
    };
    s.cron().create(&due).await.expect("test result");
    s.cron().create(&future).await.expect("test result");
    let claimed = s.cron().claim_due(now, 5).await.expect("test result");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, due.id);
    let again = s.cron().claim_due(now, 5).await.expect("test result");
    assert!(again.is_empty());
}

#[tokio::test]
async fn finish_claim_clears_and_disables_run_once() {
    let s = store().await;
    let mut job = make_job(Utc::now(), MemoryScope::default());
    job.run_once = true;
    s.cron().create(&job).await.expect("test result");
    let now = Utc::now();
    let next = now + Duration::seconds(60);
    s.cron()
        .finish_claim(&job.id, now, next, true)
        .await
        .expect("test result");
    let got = s
        .cron()
        .get(&job.id)
        .await
        .expect("test result")
        .expect("test result");
    assert!(got.claimed_at.is_none());
    assert!(!got.enabled);
}

#[tokio::test]
async fn release_claim_only_preserves_last_run() {
    let s = store().await;
    let now = Utc::now();
    let mut job = make_job(now - Duration::seconds(1), MemoryScope::default());
    job.last_run = Some(now - Duration::seconds(120));
    s.cron().create(&job).await.expect("test result");
    let _ = s.cron().claim_due(now, 1).await.expect("test result");
    s.cron()
        .release_claim_only(&job.id)
        .await
        .expect("test result");
    let got = s
        .cron()
        .get(&job.id)
        .await
        .expect("test result")
        .expect("test result");
    assert!(got.claimed_at.is_none());
    assert_eq!(got.last_run, job.last_run);
    assert_eq!(got.next_run, job.next_run);
}

#[tokio::test]
async fn recover_stuck_returns_affected_ids() {
    let s = store().await;
    let mut job = make_job(Utc::now(), MemoryScope::default());
    job.claimed_at = Some(Utc::now() - Duration::hours(1));
    s.cron().create(&job).await.expect("test result");
    let recovered = s.cron().recover_stuck(60).await.expect("test result");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0], job.id);
}
