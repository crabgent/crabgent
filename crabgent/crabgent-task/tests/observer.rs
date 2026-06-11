use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use crabgent_core::ActivityEventSummary;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::tool::{Tool, ToolCtx};
use crabgent_core::{Kernel, ToolError};
use crabgent_store::Owner;
use crabgent_store::memory::MemoryTaskStore;
use crabgent_store::records::TaskStatus;
use crabgent_task::{
    TaskActivityEvent, TaskActivityKind, TaskError, TaskExecutor, TaskObserver, TaskRequest,
};
use crabgent_test_support::{StubProvider, done, tool_call, tool_use};
use serde_json::{Value, json};

/// First turn emits a tool call carrying sensitive arguments and a thought
/// signature; the second turn finishes the run. Exercises observer redaction.
fn tool_provider() -> StubProvider {
    let mut secret_call = tool_call(
        "tool-1",
        "record_secret",
        json!({"op": "record", "secret": "private arg"}),
    );
    secret_call.thought_signature = Some("private signature".to_owned());
    StubProvider::new()
        .with_tools(true)
        .responses(vec![tool_use(vec![secret_call]), done("done")])
}

struct SecretTool;

#[async_trait]
impl Tool for SecretTool {
    fn name(&self) -> &'static str {
        "record_secret"
    }

    fn description(&self) -> &'static str {
        "record secret"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(json!({"value": "private output"}))
    }
}

#[derive(Default)]
struct RecordingObserver {
    events: Mutex<Vec<TaskActivityEvent>>,
}

#[async_trait]
impl TaskObserver for RecordingObserver {
    async fn observe(&self, event: TaskActivityEvent) -> Result<(), TaskError> {
        self.events.lock().expect("observer mutex").push(event);
        Ok(())
    }
}

impl RecordingObserver {
    fn events(&self) -> Vec<TaskActivityEvent> {
        self.events.lock().expect("observer mutex").clone()
    }
}

struct FailingObserver;

#[async_trait]
impl TaskObserver for FailingObserver {
    async fn observe(&self, _event: TaskActivityEvent) -> Result<(), TaskError> {
        Err(TaskError::notify("observer down"))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observer_receives_bounded_activity_events() {
    let store = Arc::new(MemoryTaskStore::default());
    let observer = Arc::new(RecordingObserver::default());
    let kernel = Arc::new(
        Kernel::builder()
            .provider(tool_provider())
            .add_tool(SecretTool)
            .policy(AllowAllPolicy)
            .build(),
    );
    let observer_dyn: Arc<dyn TaskObserver> = observer.clone();
    let exec = TaskExecutor::new(Arc::clone(&store))
        .with_timeout(Duration::from_secs(5))
        .with_progress_debounce(Duration::from_millis(10))
        .with_observer(observer_dyn);
    let req = TaskRequest::new(Owner::new("alice"), "m", "say hi").with_max_turns(3);

    let task = exec
        .spawn_blocking(Arc::clone(&kernel), req, None)
        .await
        .expect("spawn blocking returns task");

    assert_eq!(task.status, TaskStatus::Done);
    let events = observer.events();
    let labels = events.iter().map(activity_label).collect::<Vec<_>>();
    assert_activity_order(&labels);
    assert_eq!(events.first().expect("started event").meta.task_id, task.id);
    assert!(
        events
            .iter()
            .all(|event| event.meta.prompt_bytes == "say hi".len())
    );
    assert_tool_observer_events_are_redacted(&events);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failing_observer_does_not_fail_task() {
    let store = Arc::new(MemoryTaskStore::default());
    let kernel = Arc::new(
        Kernel::builder()
            .provider(StubProvider::with_text("hello world"))
            .policy(AllowAllPolicy)
            .build(),
    );
    let exec = TaskExecutor::new(Arc::clone(&store))
        .with_timeout(Duration::from_secs(5))
        .with_observer(Arc::new(FailingObserver));

    let task = exec
        .spawn_blocking(
            Arc::clone(&kernel),
            TaskRequest::new(Owner::new("u"), "m", "p"),
            None,
        )
        .await
        .expect("observer errors must not fail task");

    assert_eq!(task.status, TaskStatus::Done);
    assert_eq!(task.output, "hello world");
}

const fn activity_label(event: &TaskActivityEvent) -> &'static str {
    match &event.kind {
        TaskActivityKind::Started => "started",
        TaskActivityKind::Kernel(ActivityEventSummary::OutputDelta(_)) => "output_delta",
        TaskActivityKind::Kernel(ActivityEventSummary::ReasoningDelta(_)) => "reasoning_delta",
        TaskActivityKind::Kernel(ActivityEventSummary::ToolCallStarted(_)) => "tool_started",
        TaskActivityKind::Kernel(ActivityEventSummary::ToolCallCompleted(_)) => "tool_completed",
        TaskActivityKind::Kernel(ActivityEventSummary::Notification(_)) => "notification",
        TaskActivityKind::Kernel(ActivityEventSummary::ServerToolResult(_)) => "server_tool",
        TaskActivityKind::Kernel(ActivityEventSummary::AttemptFailed(_)) => "attempt_failed",
        TaskActivityKind::Kernel(ActivityEventSummary::Final(_)) => "final",
        TaskActivityKind::Completed => "completed",
        TaskActivityKind::Failed { .. } => "failed",
        TaskActivityKind::Cancelled => "cancelled",
        TaskActivityKind::TimedOut => "timed_out",
        _ => "unknown",
    }
}

fn assert_activity_order(labels: &[&'static str]) {
    let started = activity_index(labels, "started");
    let tool_started = activity_index(labels, "tool_started");
    let tool_completed = activity_index(labels, "tool_completed");
    let output_delta = activity_index(labels, "output_delta");
    let final_event = activity_index(labels, "final");
    let completed = activity_index(labels, "completed");

    assert!(started < tool_started);
    assert!(tool_started < tool_completed);
    assert!(tool_completed < output_delta);
    assert!(output_delta < final_event);
    assert!(final_event < completed);
}

fn activity_index(labels: &[&'static str], needle: &'static str) -> usize {
    labels
        .iter()
        .position(|label| *label == needle)
        .expect("activity event should be present")
}

fn assert_tool_observer_events_are_redacted(events: &[TaskActivityEvent]) {
    for event in events {
        let TaskActivityKind::Kernel(
            summary @ (ActivityEventSummary::ToolCallStarted(_)
            | ActivityEventSummary::ToolCallCompleted(_)),
        ) = &event.kind
        else {
            continue;
        };
        let encoded = serde_json::to_string(summary).expect("activity kind serializes");
        assert!(!encoded.contains("private arg"));
        assert!(!encoded.contains("private output"));
        assert!(!encoded.contains("private signature"));
    }
}
