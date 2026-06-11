use std::sync::Arc;

use super::*;
use crate::backend::SqliteStore;
use crabgent_store::Store;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::Barrier;
use uuid::Uuid;

pub(super) async fn store() -> SqliteStore {
    SqliteStore::open_in_memory().await.expect("open store")
}

async fn concurrent_store() -> (SqliteStore, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "crabgent-task-recover-{}.db",
        Uuid::now_v7().simple()
    ));
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(opts)
        .await
        .expect("open pooled sqlite store");
    (
        SqliteStore::from_pool(pool).await.expect("migrate store"),
        path,
    )
}

pub(super) fn make_task(now: DateTime<Utc>) -> Task {
    Task {
        resume_spec: None,
        resume_count: 0,
        pause_cause: None,
        paused_at: None,
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
    }
}

#[tokio::test]
async fn insert_and_get() {
    let s = store().await;
    let task = make_task(Utc::now());
    s.task().insert(&task).await.expect("test result");
    let got = s
        .task()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.prompt, "do");
}

#[tokio::test]
async fn duplicate_insert_conflicts() {
    let s = store().await;
    let task = make_task(Utc::now());
    s.task().insert(&task).await.expect("test result");
    let err = s.task().insert(&task).await.expect_err("expected error");
    assert!(matches!(err, StoreError::Conflict(_)));
}

#[tokio::test]
async fn append_output_concatenates_chunks() {
    let s = store().await;
    let task = make_task(Utc::now());
    s.task().insert(&task).await.expect("test result");
    s.task()
        .append_output(&task.id, "hi ")
        .await
        .expect("test result");
    s.task()
        .append_output(&task.id, "there")
        .await
        .expect("test result");
    let got = s
        .task()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.output, "hi there");
}

#[tokio::test]
async fn finish_marks_done() {
    let s = store().await;
    let task = make_task(Utc::now());
    s.task().insert(&task).await.expect("test result");
    s.task()
        .finish(&task.id, TaskStatus::Done, None)
        .await
        .expect("test result");
    let got = s
        .task()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.status, TaskStatus::Done);
    assert!(got.finished_at.is_some());
}

#[tokio::test]
async fn finish_rejects_running_status() {
    let s = store().await;
    let task = make_task(Utc::now());
    s.task().insert(&task).await.expect("test result");
    let err = s
        .task()
        .finish(&task.id, TaskStatus::Running, None)
        .await
        .expect_err("expected error");
    assert!(matches!(err, StoreError::Invalid(_)));
}

#[tokio::test]
async fn list_running_excludes_finished() {
    let s = store().await;
    let a = make_task(Utc::now());
    let b = make_task(Utc::now());
    s.task().insert(&a).await.expect("test result");
    s.task().insert(&b).await.expect("test result");
    s.task()
        .finish(&a.id, TaskStatus::Done, None)
        .await
        .expect("test result");
    let listed = s
        .task()
        .list_running(Page::first(10))
        .await
        .expect("test result");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, b.id);
}

#[tokio::test]
async fn list_by_owner_filters_before_pagination() {
    let s = store().await;
    let target_owner = Owner::new("target");
    let other_owner = Owner::new("other");
    let base = Utc::now() - Duration::minutes(2);

    for index in 0..60 {
        let mut task = make_task(base + Duration::seconds(index));
        task.owner = other_owner.clone();
        s.task().insert(&task).await.expect("test result");
    }
    let mut expected = Vec::new();
    for index in 60..63 {
        let mut task = make_task(base + Duration::seconds(index));
        task.owner = target_owner.clone();
        expected.push(task.id.clone());
        s.task().insert(&task).await.expect("test result");
    }

    let listed = s
        .task()
        .list_by_owner(Some(&target_owner), Page::first(2))
        .await
        .expect("test result");

    let listed_ids: Vec<_> = listed.into_iter().map(|task| task.id).collect();
    let expected_ids: Vec<_> = expected.into_iter().take(2).collect();
    assert_eq!(listed_ids, expected_ids);
}

#[tokio::test]
async fn recover_stuck_resets_old_running() {
    let s = store().await;
    let mut task = make_task(Utc::now() - Duration::hours(1));
    task.updated_at = Utc::now() - Duration::hours(1);
    s.task().insert(&task).await.expect("test result");
    let recovered = s.task().recover_stuck(60).await.expect("test result");
    assert_eq!(recovered, vec![task.id.clone()]);
    let got = s
        .task()
        .get(&task.id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(got.status, TaskStatus::Failed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recover_stuck_concurrent_returns_only_updated_rows() {
    let (s, path) = concurrent_store().await;
    let old = Utc::now() - Duration::hours(1);
    let fresh = Utc::now();

    let old_running = make_task(old);
    let fresh_running = make_task(fresh);
    let mut old_done = make_task(old);
    old_done.status = TaskStatus::Done;
    old_done.finished_at = Some(old);

    s.task().insert(&old_running).await.expect("test result");
    s.task().insert(&fresh_running).await.expect("test result");
    s.task().insert(&old_done).await.expect("test result");

    let barrier = Arc::new(Barrier::new(2));
    let left_store = s.clone();
    let left_barrier = Arc::clone(&barrier);
    let left = tokio::spawn(async move {
        left_barrier.wait().await;
        left_store.task().recover_stuck(60).await
    });
    let right_store = s.clone();
    let right_barrier = Arc::clone(&barrier);
    let right = tokio::spawn(async move {
        right_barrier.wait().await;
        right_store.task().recover_stuck(60).await
    });

    let left_recovered = left
        .await
        .expect("recover task joined")
        .expect("test result");
    let right_recovered = right
        .await
        .expect("recover task joined")
        .expect("test result");
    let mut recovered = left_recovered;
    recovered.extend(right_recovered);

    let old_running_count = recovered.iter().filter(|id| **id == old_running.id).count();
    assert_eq!(old_running_count, 1);
    assert!(!recovered.contains(&fresh_running.id));
    assert!(!recovered.contains(&old_done.id));

    let got_old = s
        .task()
        .get(&old_running.id)
        .await
        .expect("test result")
        .expect("old running task exists");
    let got_fresh = s
        .task()
        .get(&fresh_running.id)
        .await
        .expect("test result")
        .expect("fresh running task exists");
    let got_done = s
        .task()
        .get(&old_done.id)
        .await
        .expect("test result")
        .expect("done task exists");

    assert_eq!(got_old.status, TaskStatus::Failed);
    assert_eq!(got_fresh.status, TaskStatus::Running);
    assert_eq!(got_done.status, TaskStatus::Done);

    drop(s);
    std::fs::remove_file(path).expect("remove sqlite test db");
}

#[tokio::test]
async fn cleanup_old_removes_finished_tasks() {
    let s = store().await;
    let mut task = make_task(Utc::now() - Duration::days(30));
    task.status = TaskStatus::Done;
    task.finished_at = Some(Utc::now() - Duration::days(30));
    s.task().insert(&task).await.expect("test result");
    let removed = s.task().cleanup_old(7).await.expect("test result");
    assert_eq!(removed, 1);
}

pub(super) fn user_message(text: &str) -> crabgent_core::Message {
    crabgent_core::Message::User {
        content: vec![crabgent_core::ContentBlock::Text { text: text.into() }],
        timestamp: None,
    }
}
