//! Tests for [`TaskTranscriptHook`].

use super::*;
use chrono::Utc;
use crabgent_core::types::ToolCall;
use crabgent_core::{ContentBlock, RunId, Subject};
use crabgent_store::memory::MemoryTaskStore;
use crabgent_store::records::{Task, TaskStatus};
use crabgent_store::{Owner, TaskId};
use serde_json::json;

fn task_record(id: &TaskId) -> Task {
    let now = Utc::now();
    Task {
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
        resume_spec: None,
        resume_count: 0,
        pause_cause: None,
        paused_at: None,
    }
}

fn user(text: &str) -> Message {
    Message::User {
        content: vec![ContentBlock::Text { text: text.into() }],
        timestamp: None,
    }
}

fn assistant(text: &str) -> Message {
    Message::Assistant {
        text: text.into(),
        tool_calls: vec![],
    }
}

fn assistant_with_call(text: &str) -> Message {
    Message::Assistant {
        text: text.into(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            args: json!({}),
            thought_signature: None,
        }],
    }
}

fn tool_result(call_id: &str) -> Message {
    Message::ToolResult {
        call_id: call_id.into(),
        output: json!("ok"),
        is_error: false,
    }
}

async fn ready() -> (
    Arc<MemoryTaskStore>,
    TaskTranscriptHook<MemoryTaskStore>,
    TaskId,
) {
    let store = Arc::new(MemoryTaskStore::default());
    let id = TaskId::new();
    store.insert(&task_record(&id)).await.expect("insert");
    let hook = TaskTranscriptHook::new(Arc::clone(&store));
    (store, hook, id)
}

fn task_ctx(id: &TaskId) -> RunCtx {
    RunCtx::new(
        RunId::new(),
        Subject::new("u").with_attr(TASK_ID_ATTR, id.to_string()),
    )
}

#[tokio::test]
async fn persists_on_assistant_and_tool_result_appends() {
    let (store, hook, id) = ready().await;
    let ctx = task_ctx(&id);

    let log = vec![user("hi"), assistant_with_call("working")];
    hook.on_message(&log, &ctx).await;
    let saved = store
        .load_transcript(&id)
        .await
        .expect("load")
        .expect("present");
    assert_eq!(saved.len(), 2);

    let log = vec![
        user("hi"),
        assistant_with_call("working"),
        tool_result("c1"),
    ];
    hook.on_message(&log, &ctx).await;
    let saved = store
        .load_transcript(&id)
        .await
        .expect("load")
        .expect("present");
    assert_eq!(saved.len(), 3, "full overwrite with the longer log");
}

#[tokio::test]
async fn persists_initial_context_burst_before_first_assistant() {
    let (store, hook, id) = ready().await;
    let ctx = task_ctx(&id);

    let log = vec![user("context-1"), user("context-2")];
    hook.on_message(&log, &ctx).await;
    let saved = store
        .load_transcript(&id)
        .await
        .expect("load")
        .expect("present");
    assert_eq!(saved.len(), 2, "pre-turn context is durable");

    // After the first assistant message, plain user appends no longer flush.
    let log = vec![user("context-1"), assistant("a"), user("follow-up")];
    hook.on_message(&log, &ctx).await;
    let saved = store
        .load_transcript(&id)
        .await
        .expect("load")
        .expect("present");
    assert_eq!(saved.len(), 2, "user tail after a turn does not flush");
}

#[tokio::test]
async fn ignores_runs_without_task_attr() {
    let (store, hook, id) = ready().await;
    let ctx = RunCtx::new(RunId::new(), Subject::new("u"));
    hook.on_message(&[user("hi"), assistant("a")], &ctx).await;
    assert!(
        store.load_transcript(&id).await.expect("load").is_none(),
        "non-task runs never write transcripts"
    );
}

#[tokio::test]
async fn store_failure_is_fail_soft() {
    struct FailingStore;
    #[async_trait]
    impl TaskStore for FailingStore {
        async fn insert(&self, _task: &Task) -> Result<(), crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn get(&self, _id: &TaskId) -> Result<Option<Task>, crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn append_output(
            &self,
            _id: &TaskId,
            _chunk: &str,
        ) -> Result<(), crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn finish(
            &self,
            _id: &TaskId,
            _status: TaskStatus,
            _error: Option<&str>,
        ) -> Result<(), crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn pause(
            &self,
            _id: &TaskId,
            _cause: crabgent_store::TaskPauseCause,
        ) -> Result<bool, crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn claim_for_resume(
            &self,
            _id: &TaskId,
            _max_resumes: u32,
        ) -> Result<bool, crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn list_paused(
            &self,
            _page: crabgent_store::Page,
        ) -> Result<Vec<Task>, crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn pause_orphans(
            &self,
            _stale_secs: i64,
        ) -> Result<Vec<TaskId>, crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn list_children(
            &self,
            _parent: &TaskId,
            _page: crabgent_store::Page,
        ) -> Result<Vec<Task>, crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn save_transcript(
            &self,
            _id: &TaskId,
            _messages: &[Message],
        ) -> Result<(), crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn load_transcript(
            &self,
            _id: &TaskId,
        ) -> Result<Option<Vec<Message>>, crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn list_running(
            &self,
            _page: crabgent_store::Page,
        ) -> Result<Vec<Task>, crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn recover_stuck(
            &self,
            _timeout_secs: i64,
        ) -> Result<Vec<TaskId>, crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
        async fn cleanup_old(&self, _days: i64) -> Result<u64, crabgent_store::StoreError> {
            Err(crabgent_store::StoreError::backend("down"))
        }
    }

    let hook = TaskTranscriptHook::new(Arc::new(FailingStore));
    let ctx = task_ctx(&TaskId::new());
    let decision = hook.on_message(&[user("hi"), assistant("a")], &ctx).await;
    assert!(matches!(decision, Decision::Continue));
}

#[tokio::test]
async fn unparsable_task_id_is_skipped() {
    let (store, hook, id) = ready().await;
    let ctx = RunCtx::new(
        RunId::new(),
        Subject::new("u").with_attr(TASK_ID_ATTR, "not-a-task-id"),
    );
    hook.on_message(&[user("hi"), assistant("a")], &ctx).await;
    assert!(store.load_transcript(&id).await.expect("load").is_none());
}
