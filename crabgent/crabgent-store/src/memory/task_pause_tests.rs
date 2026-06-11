//! Pause/resume/transcript tests for [`MemoryTaskStore`] (sibling file
//! keeps `task_tests.rs` under the 500-line cap).

use super::tests::{make_task, user_message};
use super::*;
use chrono::TimeZone;

#[tokio::test]
async fn finish_clears_pause_fields() {
    let store = MemoryTaskStore::default();
    let task = make_task(Utc::now());
    store.insert(&task).await.expect("test result");
    assert!(
        store
            .pause(&task.id, TaskPauseCause::Shutdown)
            .await
            .expect("test result")
    );
    assert!(
        store
            .claim_for_resume(&task.id, 3)
            .await
            .expect("test result")
    );
    store
        .finish(&task.id, TaskStatus::Done, None)
        .await
        .expect("test result");
    let got = store
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert!(got.pause_cause.is_none());
    assert!(got.paused_at.is_none());
}

#[tokio::test]
async fn pause_transitions_only_from_running() {
    let store = MemoryTaskStore::default();
    let task = make_task(Utc::now());
    store.insert(&task).await.expect("test result");

    assert!(
        store
            .pause(&task.id, TaskPauseCause::Shutdown)
            .await
            .expect("test result")
    );
    let got = store
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.status, TaskStatus::Paused);
    assert_eq!(got.pause_cause, Some(TaskPauseCause::Shutdown));
    assert!(got.paused_at.is_some());

    // Second pause is a no-op (already paused).
    assert!(
        !store
            .pause(&task.id, TaskPauseCause::Forced)
            .await
            .expect("test result")
    );
    // Missing task pauses nothing.
    assert!(
        !store
            .pause(&TaskId::new(), TaskPauseCause::Shutdown)
            .await
            .expect("test result")
    );

    let done = make_task(Utc::now());
    store.insert(&done).await.expect("test result");
    store
        .finish(&done.id, TaskStatus::Done, None)
        .await
        .expect("test result");
    assert!(
        !store
            .pause(&done.id, TaskPauseCause::Shutdown)
            .await
            .expect("test result")
    );
}

#[tokio::test]
async fn claim_for_resume_claims_only_paused_and_respects_cap() {
    let store = MemoryTaskStore::default();
    let task = make_task(Utc::now());
    store.insert(&task).await.expect("test result");

    // Running task is not claimable.
    assert!(
        !store
            .claim_for_resume(&task.id, 3)
            .await
            .expect("test result")
    );

    assert!(
        store
            .pause(&task.id, TaskPauseCause::Crash)
            .await
            .expect("test result")
    );
    assert!(
        store
            .claim_for_resume(&task.id, 3)
            .await
            .expect("test result")
    );
    let got = store
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.status, TaskStatus::Running);
    assert_eq!(got.resume_count, 1);
    assert!(got.pause_cause.is_none());
    assert!(got.paused_at.is_none());

    // Cap reached: pause again, cap of 1 rejects the second claim.
    assert!(
        store
            .pause(&task.id, TaskPauseCause::Crash)
            .await
            .expect("test result")
    );
    assert!(
        !store
            .claim_for_resume(&task.id, 1)
            .await
            .expect("test result")
    );
    let got = store
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.status, TaskStatus::Paused);
    assert_eq!(got.resume_count, 1);
}

#[tokio::test]
async fn shutdown_pause_claims_bypass_and_skip_the_cap() {
    let store = MemoryTaskStore::default();
    let task = make_task(Utc::now());
    store.insert(&task).await.expect("test result");

    // A clean shutdown pause is claimable even at cap 0 and does not
    // increment the counter: graceful deploys never burn resume budget.
    assert!(
        store
            .pause(&task.id, TaskPauseCause::Shutdown)
            .await
            .expect("test result")
    );
    assert!(
        store
            .claim_for_resume(&task.id, 0)
            .await
            .expect("test result")
    );
    let got = store
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.resume_count, 0);
}

#[tokio::test]
async fn concurrent_claims_yield_exactly_one_winner() {
    let store = std::sync::Arc::new(MemoryTaskStore::default());
    let task = make_task(Utc::now());
    store.insert(&task).await.expect("test result");
    assert!(
        store
            .pause(&task.id, TaskPauseCause::Crash)
            .await
            .expect("test result")
    );

    let (a, b) = tokio::join!(
        store.claim_for_resume(&task.id, 3),
        store.claim_for_resume(&task.id, 3),
    );
    let wins = [a.expect("test result"), b.expect("test result")]
        .iter()
        .filter(|w| **w)
        .count();
    assert_eq!(wins, 1);
    let got = store
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.resume_count, 1);
}

#[tokio::test]
async fn list_paused_filters_and_orders() {
    let store = MemoryTaskStore::default();
    let a = make_task(Utc::now());
    let b = make_task(Utc::now() + Duration::seconds(1));
    let c = make_task(Utc::now() + Duration::seconds(2));
    for t in [&a, &b, &c] {
        store.insert(t).await.expect("test result");
    }
    assert!(
        store
            .pause(&c.id, TaskPauseCause::Shutdown)
            .await
            .expect("test result")
    );
    assert!(
        store
            .pause(&b.id, TaskPauseCause::Shutdown)
            .await
            .expect("test result")
    );

    let listed = store.list_paused(Page::first(10)).await.expect("listed");
    assert_eq!(
        listed.iter().map(|t| &t.id).collect::<Vec<_>>(),
        vec![&b.id, &c.id],
        "oldest first by created_at"
    );
}

#[tokio::test]
async fn pause_orphans_folds_stale_running_into_paused_crash() {
    let store = MemoryTaskStore::default();
    let old_time = Utc
        .with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
        .single()
        .expect("valid test datetime");
    let mut stale = make_task(old_time);
    stale.updated_at = old_time;
    let fresh = make_task(Utc::now());
    store.insert(&stale).await.expect("test result");
    store.insert(&fresh).await.expect("test result");
    let mut paused = make_task(old_time);
    paused.updated_at = old_time;
    store.insert(&paused).await.expect("test result");
    assert!(
        store
            .pause(&paused.id, TaskPauseCause::Shutdown)
            .await
            .expect("test result")
    );

    let adopted = store.pause_orphans(60).await.expect("test result");

    assert_eq!(adopted, vec![stale.id.clone()]);
    let got = store
        .get(&stale.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.status, TaskStatus::Paused);
    assert_eq!(got.pause_cause, Some(TaskPauseCause::Crash));
    // Fresh running task untouched; already-paused task keeps its cause.
    let fresh_got = store
        .get(&fresh.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(fresh_got.status, TaskStatus::Running);
    let paused_got = store
        .get(&paused.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(paused_got.pause_cause, Some(TaskPauseCause::Shutdown));
}

#[tokio::test]
async fn list_children_returns_direct_children_oldest_first() {
    let store = MemoryTaskStore::default();
    let parent = make_task(Utc::now());
    let mut child_b = make_task(Utc::now() + Duration::seconds(2));
    child_b.parent_task_id = Some(parent.id.clone());
    let mut child_a = make_task(Utc::now() + Duration::seconds(1));
    child_a.parent_task_id = Some(parent.id.clone());
    let unrelated = make_task(Utc::now());
    for t in [&parent, &child_a, &child_b, &unrelated] {
        store.insert(t).await.expect("test result");
    }

    let children = store
        .list_children(&parent.id, Page::first(10))
        .await
        .expect("test result");
    assert_eq!(
        children.iter().map(|t| &t.id).collect::<Vec<_>>(),
        vec![&child_a.id, &child_b.id],
    );
}

#[tokio::test]
async fn transcript_round_trips_and_bumps_heartbeat() {
    let store = MemoryTaskStore::default();
    let old_time = Utc
        .with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
        .single()
        .expect("valid test datetime");
    let mut task = make_task(old_time);
    task.updated_at = old_time;
    store.insert(&task).await.expect("test result");

    assert!(
        store
            .load_transcript(&task.id)
            .await
            .expect("test result")
            .is_none()
    );

    let messages = vec![user_message("first"), user_message("second")];
    store
        .save_transcript(&task.id, &messages)
        .await
        .expect("test result");

    let loaded = store
        .load_transcript(&task.id)
        .await
        .expect("test result")
        .expect("transcript present");
    assert_eq!(loaded.len(), 2);
    let got = store
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert!(
        got.updated_at > old_time,
        "save_transcript bumps the liveness heartbeat"
    );

    // Full overwrite, not append.
    store
        .save_transcript(&task.id, &[user_message("only")])
        .await
        .expect("test result");
    let loaded = store
        .load_transcript(&task.id)
        .await
        .expect("test result")
        .expect("transcript present");
    assert_eq!(loaded.len(), 1);
}

#[tokio::test]
async fn save_transcript_on_missing_task_is_noop() {
    let store = MemoryTaskStore::default();
    let id = TaskId::new();
    store
        .save_transcript(&id, &[user_message("x")])
        .await
        .expect("test result");
    assert!(
        store
            .load_transcript(&id)
            .await
            .expect("test result")
            .is_none()
    );
}

#[tokio::test]
async fn cleanup_old_removes_finished_tasks_and_their_transcripts() {
    let store = MemoryTaskStore::default();
    let old_time = Utc
        .with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
        .single()
        .expect("valid test datetime");
    let mut t = make_task(old_time);
    store.insert(&t).await.expect("test result");
    store
        .save_transcript(&t.id, &[user_message("x")])
        .await
        .expect("test result");
    t.status = TaskStatus::Done;
    t.finished_at = Some(old_time);
    {
        let mut tasks = store.lock().expect("test lock");
        tasks.insert(t.id.clone(), t.clone());
    }

    let removed = store.cleanup_old(7).await.expect("test result");

    assert_eq!(removed, 1);
    assert!(
        store
            .load_transcript(&t.id)
            .await
            .expect("test result")
            .is_none(),
        "transcript removed with the task row"
    );
}
