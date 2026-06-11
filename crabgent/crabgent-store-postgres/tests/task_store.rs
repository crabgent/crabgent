//! `TaskStore` integration tests.
//!
//! Each test uses `postgres_test_ctx()`. Container mode gets a fresh database;
//! `PG_TEST_DSN` mode stays idempotent through UUID primary keys.

#[path = "../src/test_helpers.rs"]
mod test_helpers;

use chrono::{Duration, Utc};
use crabgent_core::Owner;
use crabgent_store::{Page, StoreError, Task, TaskId, TaskStatus, TaskStore};
use crabgent_store_postgres::PostgresStore;
use test_helpers::postgres_test_ctx;
use uuid::Uuid;

fn owner(test_name: &str) -> Owner {
    Owner::new(format!("pg-task-{test_name}-{}", Uuid::now_v7()))
}

fn make_task(test_name: &str) -> Task {
    let now = Utc::now();
    Task {
        resume_spec: None,
        resume_count: 0,
        pause_cause: None,
        paused_at: None,
        id: TaskId::new(),
        owner: owner(test_name),
        name: None,
        prompt: "run task".into(),
        status: TaskStatus::Running,
        output: String::new(),
        error: None,
        created_at: now,
        updated_at: now,
        finished_at: None,
        parent_session_id: None,
        parent_task_id: None,
        context_mode: None,
        reasoning_effort_override: None,
    }
}

#[tokio::test]
async fn task_insert_get_roundtrip() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let task = make_task("insert-get");

    store.task_store().insert(&task).await.expect("test result");
    let got = store
        .task_store()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("task exists");

    assert_eq!(got.id, task.id);
    assert_eq!(got.owner, task.owner);
    assert_eq!(got.prompt, "run task");
}

#[tokio::test]
async fn task_duplicate_insert_conflicts() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let task = make_task("duplicate");

    store.task_store().insert(&task).await.expect("test result");
    let err = store
        .task_store()
        .insert(&task)
        .await
        .expect_err("expected error");

    assert!(matches!(err, StoreError::Conflict(_)));
}

#[tokio::test]
async fn task_append_output_concatenates_chunks() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let task = make_task("append");
    store.task_store().insert(&task).await.expect("test result");

    store
        .task_store()
        .append_output(&task.id, "alpha ")
        .await
        .expect("test result");
    store
        .task_store()
        .append_output(&task.id, "beta")
        .await
        .expect("test result");
    let got = store
        .task_store()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");

    assert_eq!(got.output, "alpha beta");
}

#[tokio::test]
async fn task_finish_marks_done() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let task = make_task("finish");
    store.task_store().insert(&task).await.expect("test result");

    store
        .task_store()
        .finish(&task.id, TaskStatus::Done, None)
        .await
        .expect("test result");
    let got = store
        .task_store()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");

    assert_eq!(got.status, TaskStatus::Done);
    assert!(got.finished_at.is_some());
}

#[tokio::test]
async fn task_finish_rejects_running_status() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let task = make_task("finish-running");
    store.task_store().insert(&task).await.expect("test result");

    let err = store
        .task_store()
        .finish(&task.id, TaskStatus::Running, None)
        .await
        .expect_err("expected error");

    assert!(matches!(err, StoreError::Invalid(_)));
}

#[tokio::test]
async fn task_list_running_excludes_finished() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let mut running = make_task("list-running");
    let mut done = make_task("list-done");
    running.created_at = Utc::now() - Duration::seconds(5);
    done.created_at = Utc::now() - Duration::seconds(4);
    store
        .task_store()
        .insert(&running)
        .await
        .expect("test result");
    store.task_store().insert(&done).await.expect("test result");
    store
        .task_store()
        .finish(&done.id, TaskStatus::Done, None)
        .await
        .expect("test result");

    let listed = store
        .task_store()
        .list_running(Page::first(1000))
        .await
        .expect("test result");

    assert!(listed.iter().any(|task| task.id == running.id));
    assert!(!listed.iter().any(|task| task.id == done.id));
}

#[tokio::test]
async fn task_list_by_owner_filters_before_pagination() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let target_owner = owner("list-owner-target");
    let other_owner = owner("list-owner-other");
    let base = Utc::now() - Duration::minutes(2);

    for index in 0..60 {
        let mut task = make_task("list-owner-other");
        task.owner = other_owner.clone();
        task.created_at = base + Duration::seconds(index);
        store.task_store().insert(&task).await.expect("test result");
    }
    let mut expected = Vec::new();
    for index in 60..63 {
        let mut task = make_task("list-owner-target");
        task.owner = target_owner.clone();
        task.created_at = base + Duration::seconds(index);
        expected.push(task.id.clone());
        store.task_store().insert(&task).await.expect("test result");
    }

    let listed = store
        .task_store()
        .list_by_owner(Some(&target_owner), Page::first(2))
        .await
        .expect("test result");

    let listed_ids: Vec<_> = listed.into_iter().map(|task| task.id).collect();
    let expected_ids: Vec<_> = expected.into_iter().take(2).collect();
    assert_eq!(listed_ids, expected_ids);
}

#[tokio::test]
async fn task_recover_stuck_resets_old_running() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let mut task = make_task("recover");
    task.updated_at = Utc::now() - Duration::hours(1);
    store.task_store().insert(&task).await.expect("test result");

    let recovered = store
        .task_store()
        .recover_stuck(60)
        .await
        .expect("test result");
    let got = store
        .task_store()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");

    assert!(recovered.contains(&task.id));
    assert_eq!(got.status, TaskStatus::Failed);
}

#[tokio::test]
async fn task_cleanup_old_removes_finished() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let mut task = make_task("cleanup");
    task.status = TaskStatus::Done;
    task.finished_at = Some(Utc::now() - Duration::days(40));
    store.task_store().insert(&task).await.expect("test result");

    let removed = store
        .task_store()
        .cleanup_old(30)
        .await
        .expect("test result");

    assert!(removed >= 1);
    assert!(
        store
            .task_store()
            .get(&task.id)
            .await
            .expect("test result")
            .is_none()
    );
}

fn user_message(text: &str) -> crabgent_core::Message {
    crabgent_core::Message::User {
        content: vec![crabgent_core::ContentBlock::Text { text: text.into() }],
        timestamp: None,
    }
}

#[tokio::test]
async fn pause_and_claim_cas_matrix() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let task = make_task("pause-claim");
    store.task_store().insert(&task).await.expect("insert");

    // Running is not claimable; pause CAS flips only from running.
    assert!(
        !store
            .task_store()
            .claim_for_resume(&task.id, 3)
            .await
            .expect("claim")
    );
    assert!(
        store
            .task_store()
            .pause(&task.id, crabgent_store::TaskPauseCause::Shutdown)
            .await
            .expect("pause")
    );
    assert!(
        !store
            .task_store()
            .pause(&task.id, crabgent_store::TaskPauseCause::Forced)
            .await
            .expect("pause again")
    );
    let got = store
        .task_store()
        .get(&task.id)
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(got.status, TaskStatus::Paused);
    assert_eq!(
        got.pause_cause,
        Some(crabgent_store::TaskPauseCause::Shutdown)
    );
    assert!(got.paused_at.is_some());

    // Claim wins once and clears pause fields. A shutdown-cause pause
    // does not burn the resume cap.
    assert!(
        store
            .task_store()
            .claim_for_resume(&task.id, 3)
            .await
            .expect("claim")
    );
    assert!(
        !store
            .task_store()
            .claim_for_resume(&task.id, 3)
            .await
            .expect("second claim")
    );
    let got = store
        .task_store()
        .get(&task.id)
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(got.status, TaskStatus::Running);
    assert_eq!(got.resume_count, 0, "shutdown claims never burn the cap");
    assert!(got.pause_cause.is_none());

    // finish rejects Paused.
    assert!(
        store
            .task_store()
            .pause(&task.id, crabgent_store::TaskPauseCause::Crash)
            .await
            .expect("pause")
    );
    let err = store
        .task_store()
        .finish(&task.id, TaskStatus::Paused, None)
        .await
        .expect_err("paused is not terminal");
    assert!(matches!(err, StoreError::Invalid(_)));
}

#[tokio::test]
async fn crash_claims_count_toward_the_cap() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let task = make_task("crash-cap");
    store.task_store().insert(&task).await.expect("insert");

    // Crash-cause claims increment the counter, and the cap rejects the
    // claim once exhausted.
    assert!(
        store
            .task_store()
            .pause(&task.id, crabgent_store::TaskPauseCause::Crash)
            .await
            .expect("pause")
    );
    assert!(
        store
            .task_store()
            .claim_for_resume(&task.id, 1)
            .await
            .expect("crash claim")
    );
    let got = store
        .task_store()
        .get(&task.id)
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(got.resume_count, 1);
    assert!(
        store
            .task_store()
            .pause(&task.id, crabgent_store::TaskPauseCause::Crash)
            .await
            .expect("pause")
    );
    assert!(
        !store
            .task_store()
            .claim_for_resume(&task.id, 1)
            .await
            .expect("capped claim")
    );
}

#[tokio::test]
async fn pause_orphans_folds_stale_running() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let stale = make_task("orphan-stale");
    store.task_store().insert(&stale).await.expect("insert");
    // Backdate the heartbeat.
    sqlx::query("UPDATE tasks SET updated_at = $1 WHERE id = $2")
        .bind(Utc::now() - Duration::hours(1))
        .bind(stale.id.as_uuid())
        .execute(&ctx.pool)
        .await
        .expect("backdate");
    let fresh = make_task("orphan-fresh");
    store.task_store().insert(&fresh).await.expect("insert");

    let adopted = store.task_store().pause_orphans(60).await.expect("adopt");

    assert!(adopted.contains(&stale.id));
    assert!(!adopted.contains(&fresh.id));
    let got = store
        .task_store()
        .get(&stale.id)
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(got.status, TaskStatus::Paused);
    assert_eq!(got.pause_cause, Some(crabgent_store::TaskPauseCause::Crash));
}

#[tokio::test]
async fn transcript_and_resume_spec_round_trip() {
    let ctx = postgres_test_ctx().await;
    let store = PostgresStore::from_pool(ctx.pool.clone());
    let mut task = make_task("transcript");
    task.resume_spec = Some(crabgent_store::TaskResumeSpec {
        subject_id: "alice".into(),
        subject_attrs: [("k".to_owned(), "v".to_owned())].into(),
        model: crabgent_store::ModelTargetDto::Provider {
            provider: "anthropic".into(),
            id: "claude-fable-5".into(),
        },
        explicit_model: None,
        session_model_override: Some("session-model".into()),
        reasoning_effort: None,
        system_prompt: None,
        max_turns: Some(9),
        tool_access: crabgent_core::ToolAccess::only(["task"]),
    });
    store.task_store().insert(&task).await.expect("insert");

    let got = store
        .task_store()
        .get(&task.id)
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(got.resume_spec, task.resume_spec);

    assert!(
        store
            .task_store()
            .load_transcript(&task.id)
            .await
            .expect("load")
            .is_none()
    );
    store
        .task_store()
        .save_transcript(&task.id, &[user_message("a"), user_message("b")])
        .await
        .expect("save");
    store
        .task_store()
        .save_transcript(&task.id, &[user_message("only")])
        .await
        .expect("overwrite");
    let loaded = store
        .task_store()
        .load_transcript(&task.id)
        .await
        .expect("load")
        .expect("present");
    assert_eq!(loaded.len(), 1, "full overwrite, not append");

    let children = store
        .task_store()
        .list_children(&task.id, Page::first(10))
        .await
        .expect("children");
    assert!(children.is_empty());

    let paused = store
        .task_store()
        .list_paused(Page::first(200))
        .await
        .expect("list paused");
    assert!(!paused.iter().any(|t| t.id == task.id));
}
