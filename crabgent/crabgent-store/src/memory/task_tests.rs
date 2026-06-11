//! Tests for [`MemoryTaskStore`], incl. the pause/resume CAS matrix.

use super::*;
use chrono::TimeZone;
use crabgent_core::{ContentBlock, Owner};

pub(super) fn make_task(now: chrono::DateTime<Utc>) -> Task {
    Task {
        id: TaskId::new(),
        owner: Owner::new("u"),
        name: None,
        prompt: "do".into(),
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
        resume_spec: None,
        resume_count: 0,
        pause_cause: None,
        paused_at: None,
    }
}

pub(super) fn user_message(text: &str) -> Message {
    Message::User {
        content: vec![ContentBlock::Text { text: text.into() }],
        timestamp: None,
    }
}

#[tokio::test]
async fn insert_and_get() {
    let store = MemoryTaskStore::default();
    let task = make_task(Utc::now());
    store.insert(&task).await.expect("test result");
    let got = store
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.prompt, "do");
}

#[tokio::test]
async fn duplicate_insert_conflicts() {
    let store = MemoryTaskStore::default();
    let task = make_task(Utc::now());
    store.insert(&task).await.expect("test result");
    let err = store.insert(&task).await.expect_err("expected error");
    assert!(matches!(err, StoreError::Conflict(_)));
}

#[tokio::test]
async fn append_output_concatenates_chunks() {
    let store = MemoryTaskStore::default();
    let task = make_task(Utc::now());
    store.insert(&task).await.expect("test result");
    store
        .append_output(&task.id, "hi ")
        .await
        .expect("test result");
    store
        .append_output(&task.id, "there")
        .await
        .expect("test result");
    let got = store
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.output, "hi there");
}

#[tokio::test]
async fn append_output_on_missing_is_noop() {
    let store = MemoryTaskStore::default();
    store
        .append_output(&TaskId::new(), "x")
        .await
        .expect("test result");
}

#[tokio::test]
async fn finish_marks_done_with_no_error() {
    let store = MemoryTaskStore::default();
    let task = make_task(Utc::now());
    store.insert(&task).await.expect("test result");
    store
        .finish(&task.id, TaskStatus::Done, None)
        .await
        .expect("test result");
    let got = store
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.status, TaskStatus::Done);
    assert!(got.finished_at.is_some());
    assert!(got.error.is_none());
}

#[tokio::test]
async fn finish_marks_failed_with_error() {
    let store = MemoryTaskStore::default();
    let task = make_task(Utc::now());
    store.insert(&task).await.expect("test result");
    store
        .finish(&task.id, TaskStatus::Failed, Some("boom"))
        .await
        .expect("test result");
    let got = store
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.status, TaskStatus::Failed);
    assert_eq!(got.error.as_deref(), Some("boom"));
}

#[tokio::test]
async fn finish_rejects_running_status() {
    let store = MemoryTaskStore::default();
    let task = make_task(Utc::now());
    store.insert(&task).await.expect("test result");
    let err = store
        .finish(&task.id, TaskStatus::Running, None)
        .await
        .expect_err("expected error");
    assert!(matches!(err, StoreError::Invalid(_)));
}

#[tokio::test]
async fn finish_rejects_paused_status() {
    let store = MemoryTaskStore::default();
    let task = make_task(Utc::now());
    store.insert(&task).await.expect("test result");
    let err = store
        .finish(&task.id, TaskStatus::Paused, None)
        .await
        .expect_err("expected error");
    assert!(matches!(err, StoreError::Invalid(_)));
}

#[tokio::test]
async fn list_running_filters_status() {
    let store = MemoryTaskStore::default();
    let a = make_task(Utc::now());
    let b = make_task(Utc::now() + Duration::seconds(1));
    store.insert(&a).await.expect("test result");
    store.insert(&b).await.expect("test result");
    store
        .finish(&a.id, TaskStatus::Done, None)
        .await
        .expect("test result");
    let listed = store
        .list_running(Page::first(10))
        .await
        .expect("test result");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, b.id);
}

#[tokio::test]
async fn list_by_owner_paginates_after_owner_filter() {
    let store = MemoryTaskStore::default();
    let now = Utc::now();
    for index in 0..60 {
        let mut task = make_task(now + Duration::seconds(index));
        task.owner = Owner::new("other");
        store.insert(&task).await.expect("test result");
    }
    let mut first = make_task(now + Duration::seconds(60));
    first.owner = Owner::new("target");
    let mut second = make_task(now + Duration::seconds(61));
    second.owner = Owner::new("target");
    store.insert(&first).await.expect("test result");
    store.insert(&second).await.expect("test result");

    let listed = store
        .list_by_owner(Some(&Owner::new("target")), Page::first(2))
        .await
        .expect("test result");

    assert_eq!(
        listed.iter().map(|task| &task.id).collect::<Vec<_>>(),
        vec![&first.id, &second.id],
    );
}

#[tokio::test]
async fn recover_stuck_marks_old_running_as_failed_and_skips_paused() {
    let store = MemoryTaskStore::default();
    let old_time = Utc
        .with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
        .single()
        .expect("valid test datetime");
    let mut old = make_task(old_time);
    old.updated_at = old_time;
    store.insert(&old).await.expect("test result");
    let mut paused = make_task(old_time);
    paused.updated_at = old_time;
    store.insert(&paused).await.expect("test result");
    assert!(
        store
            .pause(&paused.id, TaskPauseCause::Shutdown)
            .await
            .expect("test result")
    );
    // Re-stale the paused row's heartbeat to prove status is the filter.
    {
        let mut tasks = store.lock().expect("test lock");
        tasks
            .get_mut(&paused.id)
            .expect("paused task present")
            .updated_at = old_time;
    }

    let recovered = store.recover_stuck(60).await.expect("test result");

    assert_eq!(recovered, vec![old.id.clone()]);
    let got = store
        .get(&old.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.status, TaskStatus::Failed);
    let paused_got = store
        .get(&paused.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(paused_got.status, TaskStatus::Paused);
}
