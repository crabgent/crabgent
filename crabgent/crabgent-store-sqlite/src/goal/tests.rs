use super::*;
use crate::backend::SqliteStore;
use crabgent_store::{GoalStatus, Store, ThreadGoal};

async fn store() -> SqliteStore {
    SqliteStore::open_in_memory().await.expect("open store")
}

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
    let s = store().await;
    let session = SessionId::new();
    let mut g = goal(&session, Some(1_000));
    g.tokens_used = 17;
    g.time_used_seconds = 42;
    s.goal().create(&g).await.expect("create");

    let got = s.goal().get(&g.id).await.expect("get").expect("present");
    assert_eq!(got.owner, Owner::new("alice"));
    assert_eq!(got.session, session);
    assert_eq!(got.objective, "ship the feature");
    assert_eq!(got.status, GoalStatus::Active);
    assert_eq!(got.token_budget, Some(1_000));
    assert_eq!(got.tokens_used, 17);
    assert_eq!(got.time_used_seconds, 42);

    let by_session = s
        .goal()
        .get_for_session(&session)
        .await
        .expect("get_for_session")
        .expect("present");
    assert_eq!(by_session.id, g.id);
}

#[tokio::test]
async fn second_goal_for_session_conflicts_without_leaking_schema() {
    let s = store().await;
    let session = SessionId::new();
    s.goal().create(&goal(&session, None)).await.expect("first");
    let err = s
        .goal()
        .create(&goal(&session, None))
        .await
        .expect_err("second conflicts");
    let StoreError::Conflict(msg) = err else {
        panic!("expected conflict, got something else");
    };
    // Opaque message: no table or column identifiers leak.
    assert!(!msg.contains("thread_goals"), "leaked table name: {msg}");
    assert!(!msg.contains("UNIQUE"), "leaked constraint text: {msg}");
}

#[tokio::test]
async fn delete_frees_the_session_for_a_new_goal() {
    let s = store().await;
    let session = SessionId::new();
    let g = goal(&session, None);
    s.goal().create(&g).await.expect("create");
    assert!(s.goal().delete(&g.id).await.expect("delete"));
    assert!(!s.goal().delete(&g.id).await.expect("delete again"));
    s.goal()
        .create(&goal(&session, None))
        .await
        .expect("recreate after clear");
}

#[tokio::test]
async fn update_only_writes_status_and_objective() {
    let s = store().await;
    let session = SessionId::new();
    let g = goal(&session, None);
    s.goal().create(&g).await.expect("create");
    let applied = s
        .goal()
        .update(
            &g.id,
            &ThreadGoalUpdate {
                status: Some(GoalStatus::Blocked),
                objective: None,
            },
        )
        .await
        .expect("update");
    assert!(applied);
    let got = s.goal().get(&g.id).await.expect("get").expect("present");
    assert_eq!(got.status, GoalStatus::Blocked);
    assert_eq!(got.objective, "ship the feature");

    let missing = s
        .goal()
        .update(&GoalId::new(), &ThreadGoalUpdate::default())
        .await
        .expect("update missing");
    assert!(!missing);
}

#[tokio::test]
async fn account_usage_accumulates_and_flips_to_budget_limited() {
    let s = store().await;
    let session = SessionId::new();
    let g = goal(&session, Some(100));
    s.goal().create(&g).await.expect("create");

    let after = s
        .goal()
        .account_usage(&g.id, 60, 4, Utc::now())
        .await
        .expect("account")
        .expect("present");
    assert_eq!(after.tokens_used, 60);
    assert_eq!(after.time_used_seconds, 4);
    assert_eq!(after.status, GoalStatus::Active);

    let after = s
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
    let s = store().await;
    let session = SessionId::new();
    let g = goal(&session, Some(100));
    s.goal().create(&g).await.expect("create");
    let below = s
        .goal()
        .account_usage(&g.id, 99, 0, Utc::now())
        .await
        .expect("account")
        .expect("present");
    assert_eq!(below.status, GoalStatus::Active);
    let at = s
        .goal()
        .account_usage(&g.id, 1, 0, Utc::now())
        .await
        .expect("account")
        .expect("present");
    assert_eq!(at.tokens_used, 100);
    assert_eq!(at.status, GoalStatus::BudgetLimited);
}

#[tokio::test]
async fn account_usage_clamps_negatives_and_unbudgeted_never_flips() {
    let s = store().await;
    let session = SessionId::new();
    let g = goal(&session, None);
    s.goal().create(&g).await.expect("create");
    let after = s
        .goal()
        .account_usage(&g.id, -10, -3, Utc::now())
        .await
        .expect("account")
        .expect("present");
    assert_eq!(after.tokens_used, 0);
    assert_eq!(after.time_used_seconds, 0);

    let after = s
        .goal()
        .account_usage(&g.id, 9_999_999, 1, Utc::now())
        .await
        .expect("account")
        .expect("present");
    assert_eq!(after.status, GoalStatus::Active);
}

#[tokio::test]
async fn account_usage_missing_goal_returns_none() {
    let s = store().await;
    let out = s
        .goal()
        .account_usage(&GoalId::new(), 1, 1, Utc::now())
        .await
        .expect("account");
    assert!(out.is_none());
}

#[tokio::test]
async fn list_by_status_and_resume_suspended_flip_only_suspended() {
    let s = store().await;
    let suspended = goal(&SessionId::new(), None);
    let paused = goal(&SessionId::new(), None);
    s.goal().create(&suspended).await.expect("create");
    s.goal().create(&paused).await.expect("create");
    for (id, status) in [
        (&suspended.id, GoalStatus::Suspended),
        (&paused.id, GoalStatus::Paused),
    ] {
        let update = crabgent_store::ThreadGoalUpdate {
            objective: None,
            status: Some(status),
        };
        assert!(s.goal().update(id, &update).await.expect("update"));
    }

    let listed = s
        .goal()
        .list_by_status(GoalStatus::Suspended, Page::first(10))
        .await
        .expect("list");
    assert_eq!(
        listed.iter().map(|g| &g.id).collect::<Vec<_>>(),
        vec![&suspended.id]
    );

    let at = Utc::now();
    let resumed = s.goal().resume_suspended(at).await.expect("resume");
    assert_eq!(
        resumed.iter().map(|g| &g.id).collect::<Vec<_>>(),
        vec![&suspended.id]
    );
    let got = s
        .goal()
        .get(&suspended.id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got.status, GoalStatus::Active);
    let paused_got = s
        .goal()
        .get(&paused.id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(paused_got.status, GoalStatus::Paused);

    let again = s.goal().resume_suspended(Utc::now()).await.expect("resume");
    assert!(again.is_empty(), "second flip is idempotent");
}
