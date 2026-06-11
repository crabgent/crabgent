//! Pause/resume/transcript tests for `SqliteTaskStore` (sibling file
//! keeps `task_tests.rs` under the 500-line cap).

use chrono::{Duration, Utc};
use crabgent_store::{Page, Store, StoreError, TaskId, TaskPauseCause, TaskStatus, TaskStore};

use super::tests::{make_task, store, user_message};

#[tokio::test]
async fn pause_cas_transitions_only_from_running() {
    let s = store().await;
    let task = make_task(Utc::now());
    s.task().insert(&task).await.expect("test result");

    assert!(
        s.task()
            .pause(&task.id, TaskPauseCause::Shutdown)
            .await
            .expect("test result")
    );
    let got = s
        .task()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("task exists");
    assert_eq!(got.status, TaskStatus::Paused);
    assert_eq!(got.pause_cause, Some(TaskPauseCause::Shutdown));
    assert!(got.paused_at.is_some());

    assert!(
        !s.task()
            .pause(&task.id, TaskPauseCause::Forced)
            .await
            .expect("test result"),
        "second pause is a no-op"
    );
    assert!(
        !s.task()
            .pause(&TaskId::new(), TaskPauseCause::Shutdown)
            .await
            .expect("test result"),
        "missing task pauses nothing"
    );
}

#[tokio::test]
async fn claim_for_resume_cas_and_cap() {
    let s = store().await;
    let task = make_task(Utc::now());
    s.task().insert(&task).await.expect("test result");

    assert!(
        !s.task()
            .claim_for_resume(&task.id, 3)
            .await
            .expect("test result"),
        "running task is not claimable"
    );
    assert!(
        s.task()
            .pause(&task.id, TaskPauseCause::Crash)
            .await
            .expect("test result")
    );
    assert!(
        s.task()
            .claim_for_resume(&task.id, 3)
            .await
            .expect("test result")
    );
    assert!(
        !s.task()
            .claim_for_resume(&task.id, 3)
            .await
            .expect("test result"),
        "claim is single-winner"
    );
    let got = s
        .task()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("task exists");
    assert_eq!(got.status, TaskStatus::Running);
    assert_eq!(got.resume_count, 1);
    assert!(got.pause_cause.is_none());
    assert!(got.paused_at.is_none());

    assert!(
        s.task()
            .pause(&task.id, TaskPauseCause::Crash)
            .await
            .expect("test result")
    );
    assert!(
        !s.task()
            .claim_for_resume(&task.id, 1)
            .await
            .expect("test result"),
        "resume cap rejects the claim"
    );
}

#[tokio::test]
async fn shutdown_pause_claims_bypass_and_skip_the_cap() {
    let s = store().await;
    let task = make_task(Utc::now());
    s.task().insert(&task).await.expect("test result");
    assert!(
        s.task()
            .pause(&task.id, TaskPauseCause::Shutdown)
            .await
            .expect("test result")
    );
    assert!(
        s.task()
            .claim_for_resume(&task.id, 0)
            .await
            .expect("test result"),
        "shutdown pause is claimable even at cap 0"
    );
    let got = s
        .task()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("task exists");
    assert_eq!(got.resume_count, 0, "shutdown claims never burn the cap");
}

#[tokio::test]
async fn list_paused_orders_oldest_first() {
    let s = store().await;
    let a = make_task(Utc::now());
    let b = make_task(Utc::now() + Duration::seconds(1));
    for t in [&a, &b] {
        s.task().insert(t).await.expect("test result");
        assert!(
            s.task()
                .pause(&t.id, TaskPauseCause::Shutdown)
                .await
                .expect("test result")
        );
    }
    let listed = s
        .task()
        .list_paused(Page::first(10))
        .await
        .expect("test result");
    assert_eq!(
        listed.iter().map(|t| &t.id).collect::<Vec<_>>(),
        vec![&a.id, &b.id]
    );
}

#[tokio::test]
async fn pause_orphans_folds_stale_running_and_recover_stuck_skips_paused() {
    let s = store().await;
    let mut stale = make_task(Utc::now() - Duration::hours(1));
    stale.updated_at = Utc::now() - Duration::hours(1);
    let fresh = make_task(Utc::now());
    s.task().insert(&stale).await.expect("test result");
    s.task().insert(&fresh).await.expect("test result");

    let adopted = s.task().pause_orphans(60).await.expect("test result");

    assert_eq!(adopted, vec![stale.id.clone()]);
    let got = s
        .task()
        .get(&stale.id)
        .await
        .expect("test result")
        .expect("task exists");
    assert_eq!(got.status, TaskStatus::Paused);
    assert_eq!(got.pause_cause, Some(TaskPauseCause::Crash));

    // The adopted row's heartbeat is fresh, and recover_stuck only touches
    // running rows, so nothing is failed.
    let recovered = s.task().recover_stuck(60).await.expect("test result");
    assert!(recovered.is_empty());
}

#[tokio::test]
async fn list_children_filters_by_parent() {
    let s = store().await;
    let parent = make_task(Utc::now());
    let mut child = make_task(Utc::now() + Duration::seconds(1));
    child.parent_task_id = Some(parent.id.clone());
    let unrelated = make_task(Utc::now());
    for t in [&parent, &child, &unrelated] {
        s.task().insert(t).await.expect("test result");
    }
    let children = s
        .task()
        .list_children(&parent.id, Page::first(10))
        .await
        .expect("test result");
    assert_eq!(
        children.iter().map(|t| &t.id).collect::<Vec<_>>(),
        vec![&child.id]
    );
}

#[tokio::test]
async fn transcript_round_trips_with_full_overwrite_and_heartbeat() {
    let s = store().await;
    let mut task = make_task(Utc::now() - Duration::hours(1));
    task.updated_at = Utc::now() - Duration::hours(1);
    s.task().insert(&task).await.expect("test result");

    assert!(
        s.task()
            .load_transcript(&task.id)
            .await
            .expect("test result")
            .is_none()
    );

    s.task()
        .save_transcript(&task.id, &[user_message("a"), user_message("b")])
        .await
        .expect("test result");
    let loaded = s
        .task()
        .load_transcript(&task.id)
        .await
        .expect("test result")
        .expect("transcript present");
    assert_eq!(loaded.len(), 2);

    s.task()
        .save_transcript(&task.id, &[user_message("only")])
        .await
        .expect("test result");
    let loaded = s
        .task()
        .load_transcript(&task.id)
        .await
        .expect("test result")
        .expect("transcript present");
    assert_eq!(loaded.len(), 1);

    let got = s
        .task()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("task exists");
    assert!(
        got.updated_at > task.updated_at,
        "save_transcript bumps the heartbeat"
    );

    // Missing task: write is a no-op, read returns None.
    let missing = TaskId::new();
    s.task()
        .save_transcript(&missing, &[user_message("x")])
        .await
        .expect("test result");
    assert!(
        s.task()
            .load_transcript(&missing)
            .await
            .expect("test result")
            .is_none()
    );
}

#[tokio::test]
async fn resume_spec_round_trips_through_insert() {
    let s = store().await;
    let mut task = make_task(Utc::now());
    task.resume_spec = Some(crabgent_store::TaskResumeSpec {
        subject_id: "alice".into(),
        subject_attrs: [("k".to_owned(), "v".to_owned())].into(),
        model: crabgent_store::ModelTargetDto::Id("m".into()),
        explicit_model: None,
        session_model_override: None,
        reasoning_effort: None,
        system_prompt: Some("be terse".into()),
        max_turns: Some(7),
        tool_access: crabgent_core::ToolAccess::only(["task"]),
    });
    s.task().insert(&task).await.expect("test result");
    let got = s
        .task()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("task exists");
    assert_eq!(got.resume_spec, task.resume_spec);
}

#[tokio::test]
async fn finish_rejects_paused_and_clears_pause_fields() {
    let s = store().await;
    let task = make_task(Utc::now());
    s.task().insert(&task).await.expect("test result");
    let err = s
        .task()
        .finish(&task.id, TaskStatus::Paused, None)
        .await
        .expect_err("paused is not terminal");
    assert!(matches!(err, StoreError::Invalid(_)));

    assert!(
        s.task()
            .pause(&task.id, TaskPauseCause::Forced)
            .await
            .expect("test result")
    );
    assert!(
        s.task()
            .claim_for_resume(&task.id, 3)
            .await
            .expect("test result")
    );
    s.task()
        .finish(&task.id, TaskStatus::Done, None)
        .await
        .expect("test result");
    let got = s
        .task()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("task exists");
    assert!(got.pause_cause.is_none());
    assert!(got.paused_at.is_none());
}
