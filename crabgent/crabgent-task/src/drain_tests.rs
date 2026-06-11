//! Tests for the stream drain (sibling file keeps `drain.rs` under the cap).

use super::*;
use chrono::TimeZone;
use crabgent_core::types::{Notification, NotificationLevel, ToolCall, ToolResult};
use crabgent_store::memory::MemoryTaskStore;
use crabgent_store::records::{Task, TaskStatus};
use crabgent_store::{Owner, TaskId};
use futures::stream;
use serde_json::json;

fn fixture_task(id: &TaskId) -> Task {
    let now = Utc::now();
    Task {
        resume_spec: None,
        resume_count: 0,
        pause_cause: None,
        paused_at: None,
        id: id.clone(),
        owner: Owner::new("u"),
        name: None,
        prompt: "p".into(),
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

async fn ready_store() -> (Arc<MemoryTaskStore>, TaskId) {
    let store = Arc::new(MemoryTaskStore::default());
    let id = TaskId::new();
    store.insert(&fixture_task(&id)).await.expect("test result");
    (store, id)
}

fn ev_token(s: &str) -> Result<Event, KernelError> {
    Ok(Event::Token(s.to_owned()))
}

fn ev_final(s: &str) -> Result<Event, KernelError> {
    Ok(Event::Final(s.to_owned()))
}

#[tokio::test]
async fn final_event_terminates_drain_with_final_text() {
    let (store, id) = ready_store().await;
    let s = stream::iter(vec![ev_token("hi"), ev_final("done")]);
    let out = drain_stream(Arc::clone(&store), id, s, 64, Duration::from_millis(1)).await;
    assert_eq!(out.final_text.as_deref(), Some("done"));
    assert!(out.error.is_none());
    assert!(out.is_success());
}

#[tokio::test]
async fn token_chunks_persist_when_size_threshold_hits() {
    let (store, id) = ready_store().await;
    let s = stream::iter(vec![ev_token("0123456789"), ev_final("end")]);
    drain_stream(Arc::clone(&store), id.clone(), s, 4, Duration::from_mins(1)).await;
    let task = store
        .get(&id)
        .await
        .expect("test result")
        .expect("test result");
    assert!(task.output.contains("0123456789"));
}

#[tokio::test]
async fn small_chunks_below_size_threshold_still_flush_at_end() {
    let (store, id) = ready_store().await;
    let s = stream::iter(vec![ev_token("ab"), ev_final("done")]);
    drain_stream(
        Arc::clone(&store),
        id.clone(),
        s,
        1024,
        Duration::from_mins(1),
    )
    .await;
    let task = store
        .get(&id)
        .await
        .expect("test result")
        .expect("test result");
    assert!(task.output.contains("ab"));
}

#[tokio::test]
async fn stream_error_terminates_drain_with_error() {
    let (store, id) = ready_store().await;
    let s = stream::iter(vec![
        ev_token("partial"),
        Err(KernelError::HookDenied {
            reason: "bad".into(),
        }),
    ]);
    let out = drain_stream(
        Arc::clone(&store),
        id.clone(),
        s,
        1024,
        Duration::from_mins(1),
    )
    .await;
    assert!(out.final_text.is_none());
    assert!(out.error.is_some());
    assert!(!out.is_success());
    let task = store
        .get(&id)
        .await
        .expect("test result")
        .expect("test result");
    assert!(task.output.contains("partial"));
}

#[tokio::test]
async fn tool_call_events_are_not_persisted() {
    let (store, id) = ready_store().await;
    let call = ToolCall {
        id: "c1".into(),
        name: "bash".into(),
        args: json!({}),
        thought_signature: None,
    };
    let result = ToolResult {
        call_id: "c1".into(),
        output: json!("ok"),
        is_error: false,
        run_messages: Vec::new(),
    };
    let s = stream::iter(vec![
        Ok(Event::ToolCallStarted(call.clone())),
        Ok(Event::ToolCallCompleted { call, result }),
        ev_final("done"),
    ]);
    drain_stream(
        Arc::clone(&store),
        id.clone(),
        s,
        64,
        Duration::from_mins(1),
    )
    .await;
    let task = store
        .get(&id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(task.output, "");
}

#[tokio::test]
async fn notification_events_are_not_persisted() {
    let (store, id) = ready_store().await;
    let note = Notification {
        kind: "k".into(),
        message: "n".into(),
        level: NotificationLevel::Info,
    };
    let s = stream::iter(vec![Ok(Event::Notification(note)), ev_final("done")]);
    drain_stream(
        Arc::clone(&store),
        id.clone(),
        s,
        64,
        Duration::from_mins(1),
    )
    .await;
    let task = store
        .get(&id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(task.output, "");
}

#[tokio::test]
async fn empty_stream_returns_empty_outcome() {
    let (store, id) = ready_store().await;
    let s = stream::iter(Vec::<Result<Event, KernelError>>::new());
    let out = drain_stream(Arc::clone(&store), id, s, 64, Duration::from_mins(1)).await;
    // A clean close with no `Event::Final` yields both fields `None`. The
    // executor classifies this shape as `TaskStatus::Failed` (no output
    // produced); that mapping is pinned in
    // `executor::tests::classify_clean_close_without_final_yields_failed`.
    assert!(out.final_text.is_none());
    assert!(out.error.is_none());
    assert!(
        out.is_success(),
        "DrainOutcome::is_success keys off error only"
    );
}

#[tokio::test]
async fn drain_outcome_is_success_only_without_error() {
    let with_err = DrainOutcome {
        final_text: Some("x".into()),
        error: Some("y".into()),
        paused: false,
    };
    let ok = DrainOutcome {
        final_text: Some("x".into()),
        error: None,
        paused: false,
    };
    assert!(!with_err.is_success());
    assert!(ok.is_success());
}

#[tokio::test]
async fn paused_terminal_sets_flag_without_error() {
    let (store, id) = ready_store().await;
    let s = stream::iter(vec![ev_token("partial"), Err(KernelError::Paused)]);
    let out = drain_stream(
        Arc::clone(&store),
        id.clone(),
        s,
        1024,
        Duration::from_mins(1),
    )
    .await;
    assert!(out.paused);
    assert!(out.error.is_none());
    assert!(out.final_text.is_none());
    let task = store
        .get(&id)
        .await
        .expect("test result")
        .expect("test result");
    assert!(
        task.output.contains("partial"),
        "buffered output flushes before the pause exit"
    );
}

#[tokio::test]
async fn should_flush_returns_true_at_size_threshold() {
    let last = Utc
        .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .expect("valid test datetime");
    assert!(should_flush("12345", 4, last, Duration::from_mins(1)));
}

#[tokio::test]
async fn should_flush_returns_true_after_time_threshold() {
    let long_ago = Utc::now() - chrono::Duration::seconds(120);
    assert!(should_flush(
        "ab",
        1024,
        long_ago,
        Duration::from_millis(10)
    ));
}

#[tokio::test]
async fn should_flush_returns_false_when_neither_triggers() {
    let now = Utc::now();
    assert!(!should_flush("ab", 1024, now, Duration::from_mins(1)));
}

#[tokio::test]
async fn multiple_tokens_concatenate_into_output() {
    let (store, id) = ready_store().await;
    let s = stream::iter(vec![
        ev_token("a"),
        ev_token("b"),
        ev_token("c"),
        ev_final("done"),
    ]);
    drain_stream(
        Arc::clone(&store),
        id.clone(),
        s,
        1024,
        Duration::from_mins(1),
    )
    .await;
    let task = store
        .get(&id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(task.output, "abc");
}
