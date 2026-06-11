//! `GoalStore` integration tests against a real Postgres backend.
//!
//! Each test uses `postgres_test_ctx()`, which creates a fresh database in
//! container mode and `PG_TEST_DSN` mode.

#[path = "../src/test_helpers.rs"]
mod test_helpers;

use chrono::Utc;
use crabgent_store::{
    GoalId, GoalStatus, GoalStore, Owner, SessionId, Store, StoreError, ThreadGoal,
};
use crabgent_store_postgres::PostgresStore;
use test_helpers::postgres_test_ctx;

fn goal(session: &SessionId, budget: Option<i64>) -> ThreadGoal {
    ThreadGoal::new(
        Owner::new("alice"),
        session.clone(),
        "ship the feature",
        budget,
    )
}

#[tokio::test]
async fn create_get_roundtrip_preserves_all_fields() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let session = SessionId::new();
    let mut g = goal(&session, Some(1_000));
    g.tokens_used = 17;
    g.time_used_seconds = 42;
    store.goal().create(&g).await.expect("create");

    let got = store
        .goal()
        .get(&g.id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got.owner, Owner::new("alice"));
    assert_eq!(got.session, session);
    assert_eq!(got.objective, "ship the feature");
    assert_eq!(got.status, GoalStatus::Active);
    assert_eq!(got.token_budget, Some(1_000));
    assert_eq!(got.tokens_used, 17);
    assert_eq!(got.time_used_seconds, 42);

    let by_session = store
        .goal()
        .get_for_session(&session)
        .await
        .expect("get_for_session")
        .expect("present");
    assert_eq!(by_session.id, g.id);
}

#[tokio::test]
async fn second_goal_for_session_conflicts() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let session = SessionId::new();
    store
        .goal()
        .create(&goal(&session, None))
        .await
        .expect("first");
    let err = store
        .goal()
        .create(&goal(&session, None))
        .await
        .expect_err("second conflicts");
    assert!(matches!(err, StoreError::Conflict(_)));
}

#[tokio::test]
async fn delete_frees_the_session_for_a_new_goal() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let session = SessionId::new();
    let g = goal(&session, None);
    store.goal().create(&g).await.expect("create");
    assert!(store.goal().delete(&g.id).await.expect("delete"));
    assert!(!store.goal().delete(&g.id).await.expect("delete again"));
    store
        .goal()
        .create(&goal(&session, None))
        .await
        .expect("recreate after clear");
}

#[tokio::test]
async fn update_only_writes_status_and_objective() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let session = SessionId::new();
    let g = goal(&session, None);
    store.goal().create(&g).await.expect("create");
    let applied = store
        .goal()
        .update(
            &g.id,
            &crabgent_store::ThreadGoalUpdate {
                status: Some(GoalStatus::Complete),
                objective: None,
            },
        )
        .await
        .expect("update");
    assert!(applied);
    let got = store
        .goal()
        .get(&g.id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got.status, GoalStatus::Complete);
    assert_eq!(got.objective, "ship the feature");

    let missing = store
        .goal()
        .update(&GoalId::new(), &crabgent_store::ThreadGoalUpdate::default())
        .await
        .expect("update missing");
    assert!(!missing);
}

#[tokio::test]
async fn account_usage_accumulates_and_flips_to_budget_limited() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let session = SessionId::new();
    let g = goal(&session, Some(100));
    store.goal().create(&g).await.expect("create");

    let after = store
        .goal()
        .account_usage(&g.id, 60, 4, Utc::now())
        .await
        .expect("account")
        .expect("present");
    assert_eq!(after.tokens_used, 60);
    assert_eq!(after.time_used_seconds, 4);
    assert_eq!(after.status, GoalStatus::Active);

    let after = store
        .goal()
        .account_usage(&g.id, 50, 6, Utc::now())
        .await
        .expect("account")
        .expect("present");
    assert_eq!(after.tokens_used, 110);
    assert_eq!(after.time_used_seconds, 10);
    assert_eq!(after.status, GoalStatus::BudgetLimited);
}

#[tokio::test]
async fn account_usage_flips_exactly_at_budget_boundary() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let session = SessionId::new();
    let g = goal(&session, Some(100));
    store.goal().create(&g).await.expect("create");
    let below = store
        .goal()
        .account_usage(&g.id, 99, 0, Utc::now())
        .await
        .expect("account")
        .expect("present");
    assert_eq!(below.status, GoalStatus::Active);
    let at = store
        .goal()
        .account_usage(&g.id, 1, 0, Utc::now())
        .await
        .expect("account")
        .expect("present");
    assert_eq!(at.tokens_used, 100);
    assert_eq!(at.status, GoalStatus::BudgetLimited);
}

#[tokio::test]
async fn account_usage_clamps_negatives_and_handles_missing() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let session = SessionId::new();
    let g = goal(&session, None);
    store.goal().create(&g).await.expect("create");
    let after = store
        .goal()
        .account_usage(&g.id, -10, -3, Utc::now())
        .await
        .expect("account")
        .expect("present");
    assert_eq!(after.tokens_used, 0);
    assert_eq!(after.time_used_seconds, 0);

    let missing = store
        .goal()
        .account_usage(&GoalId::new(), 1, 1, Utc::now())
        .await
        .expect("account missing");
    assert!(missing.is_none());
}

#[tokio::test]
async fn list_by_status_and_resume_suspended_flip_only_suspended() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let suspended = goal(&SessionId::new(), None);
    let paused = goal(&SessionId::new(), None);
    store.goal().create(&suspended).await.expect("create");
    store.goal().create(&paused).await.expect("create");
    for (id, status) in [
        (&suspended.id, GoalStatus::Suspended),
        (&paused.id, GoalStatus::Paused),
    ] {
        let update = crabgent_store::ThreadGoalUpdate {
            objective: None,
            status: Some(status),
        };
        assert!(store.goal().update(id, &update).await.expect("update"));
    }

    let listed = store
        .goal()
        .list_by_status(GoalStatus::Suspended, crabgent_store::Page::first(200))
        .await
        .expect("list");
    assert!(listed.iter().any(|g| g.id == suspended.id));
    assert!(!listed.iter().any(|g| g.id == paused.id));

    let resumed = store
        .goal()
        .resume_suspended(Utc::now())
        .await
        .expect("resume");
    assert!(resumed.iter().any(|g| g.id == suspended.id));
    let got = store
        .goal()
        .get(&suspended.id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got.status, GoalStatus::Active);
    let paused_got = store
        .goal()
        .get(&paused.id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(paused_got.status, GoalStatus::Paused);
}
