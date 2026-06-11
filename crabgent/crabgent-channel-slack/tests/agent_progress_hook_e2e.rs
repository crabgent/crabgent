use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_channel::channel::ChannelKind;
use crabgent_channel::subject::ChannelSubjectExt;
use crabgent_channel_slack::agent_progress::types::{AgentProgressError, AgentProgressResult};
use crabgent_channel_slack::block_kit::TaskStatus;
use crabgent_channel_slack::subject::{SLACK_CHANNEL_ID, SLACK_THREAD_ROOT};
use crabgent_channel_slack::{
    NoopSlackAgentProgress, ProgressChunk, SlackAgentProgress, SlackAgentProgressHook,
};
use crabgent_core::owner::Owner;
use crabgent_core::types::{ToolCall, ToolResult};
use crabgent_core::{Decision, Event, Hook, Outcome, RunCtx, RunId, Subject};
use serde_json::json;

const CHANNEL: &str = "C1";
const THREAD: &str = "1700000000.000100";

fn slack_subject() -> Subject {
    Subject::new("agent")
        .with_channel("slack", &Owner::new("slack:T1/C1"), ChannelKind::Group)
        .with_attr(SLACK_CHANNEL_ID, CHANNEL)
        .with_attr(SLACK_THREAD_ROOT, THREAD)
}

fn slack_ctx() -> RunCtx {
    RunCtx::new(RunId::new(), slack_subject())
}

#[derive(Default)]
struct CapturingIndicator {
    starts: AtomicUsize,
    chunks: tokio::sync::Mutex<Vec<ProgressChunk>>,
    stops: AtomicUsize,
}

#[async_trait]
impl SlackAgentProgress for CapturingIndicator {
    async fn start(&self, _ctx: &RunCtx, _initial: &str) -> AgentProgressResult<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn chunk(&self, _ctx: &RunCtx, chunk: ProgressChunk) -> AgentProgressResult<()> {
        self.chunks.lock().await.push(chunk);
        Ok(())
    }
    async fn stop(&self, _ctx: &RunCtx) -> AgentProgressResult<()> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl CapturingIndicator {
    async fn chunks(&self) -> Vec<ProgressChunk> {
        self.chunks.lock().await.clone()
    }
}

#[derive(Default)]
struct FailingIndicator {
    starts: AtomicUsize,
    chunks: AtomicUsize,
    stops: AtomicUsize,
}

#[async_trait]
impl SlackAgentProgress for FailingIndicator {
    async fn start(&self, _ctx: &RunCtx, _initial: &str) -> AgentProgressResult<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Err(AgentProgressError::Transport("start boom".into()))
    }
    async fn chunk(&self, _ctx: &RunCtx, _chunk: ProgressChunk) -> AgentProgressResult<()> {
        self.chunks.fetch_add(1, Ordering::SeqCst);
        Err(AgentProgressError::Transport("chunk boom".into()))
    }
    async fn stop(&self, _ctx: &RunCtx) -> AgentProgressResult<()> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Err(AgentProgressError::Transport("stop boom".into()))
    }
}

#[tokio::test]
async fn hook_does_not_start_on_session_start() {
    let ind: Arc<CapturingIndicator> = Arc::new(CapturingIndicator::default());
    let hook = SlackAgentProgressHook::new(ind.clone());
    let decision = hook.on_session_start(&slack_ctx()).await;
    assert!(matches!(decision, Decision::Continue));
    assert_eq!(
        ind.starts.load(Ordering::SeqCst),
        0,
        "start is deferred until the first non-silent tool call",
    );
}

#[tokio::test]
async fn hook_starts_lazily_on_first_non_silent_tool() {
    let ind: Arc<CapturingIndicator> = Arc::new(CapturingIndicator::default());
    let hook = SlackAgentProgressHook::new(ind.clone());
    let rc = slack_ctx();

    let call = ToolCall {
        id: "1".into(),
        name: "calendar".into(),
        args: json!({}),
        thought_signature: None,
    };
    let _ = hook
        .on_event(&Event::ToolCallStarted(call.clone()), &rc)
        .await;
    assert_eq!(ind.starts.load(Ordering::SeqCst), 1);

    // Second non-silent tool call does not re-start the indicator.
    let other = ToolCall {
        id: "2".into(),
        name: "memory".into(),
        args: json!({}),
        thought_signature: None,
    };
    let _ = hook.on_event(&Event::ToolCallStarted(other), &rc).await;
    assert_eq!(ind.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn hook_skips_channel_send_entirely() {
    let ind: Arc<CapturingIndicator> = Arc::new(CapturingIndicator::default());
    let hook = SlackAgentProgressHook::new(ind.clone());
    let rc = slack_ctx();

    let call = ToolCall {
        id: "1".into(),
        name: "channel_send".into(),
        args: json!({}),
        thought_signature: None,
    };
    let result = ToolResult::success(json!("ok"));
    let _ = hook
        .on_event(&Event::ToolCallStarted(call.clone()), &rc)
        .await;
    let _ = hook
        .on_event(&Event::ToolCallCompleted { call, result }, &rc)
        .await;

    assert_eq!(ind.starts.load(Ordering::SeqCst), 0);
    assert!(ind.chunks().await.is_empty());
}

#[tokio::test]
async fn hook_stop_is_noop_when_never_started() {
    let ind: Arc<CapturingIndicator> = Arc::new(CapturingIndicator::default());
    let hook = SlackAgentProgressHook::new(ind.clone());
    hook.on_stop(&slack_ctx(), &Outcome::Completed("ok".into()))
        .await;
    assert_eq!(ind.stops.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn hook_forwards_tool_call_events_as_chunks() {
    let ind: Arc<CapturingIndicator> = Arc::new(CapturingIndicator::default());
    let hook = SlackAgentProgressHook::new(ind.clone());
    let rc = slack_ctx();

    let call = ToolCall {
        id: "1".into(),
        name: "search".into(),
        args: json!({}),
        thought_signature: None,
    };
    let result = ToolResult::success(json!("done"));

    let _ = hook
        .on_event(&Event::ToolCallStarted(call.clone()), &rc)
        .await;
    let _ = hook
        .on_event(&Event::ToolCallCompleted { call, result }, &rc)
        .await;

    let chunks = ind.chunks().await;
    assert_eq!(chunks.len(), 4);
    assert!(matches!(&chunks[0], ProgressChunk::Status(status) if status == "calling search"));
    let ProgressChunk::TaskUpdate(started) = &chunks[1] else {
        panic!("expected started task_update");
    };
    assert_eq!(started.id, "1");
    assert_eq!(started.title, "search");
    assert_eq!(started.status, TaskStatus::InProgress);
    assert!(matches!(&chunks[2], ProgressChunk::Status(status) if status == "search done"));
    let ProgressChunk::TaskUpdate(completed) = &chunks[3] else {
        panic!("expected completed task_update");
    };
    assert_eq!(completed.id, "1");
    assert_eq!(completed.title, "search");
    assert_eq!(completed.status, TaskStatus::Complete);
}

#[tokio::test]
async fn hook_swallows_indicator_errors() {
    let ind: Arc<FailingIndicator> = Arc::new(FailingIndicator::default());
    let hook = SlackAgentProgressHook::new(ind.clone());
    let rc = slack_ctx();

    let decision = hook.on_session_start(&rc).await;
    assert!(matches!(decision, Decision::Continue));
    assert_eq!(
        ind.starts.load(Ordering::SeqCst),
        0,
        "start stays deferred until the first non-silent tool fires",
    );

    let call = ToolCall {
        id: "1".into(),
        name: "search".into(),
        args: json!({}),
        thought_signature: None,
    };
    let started = hook
        .on_event(&Event::ToolCallStarted(call.clone()), &rc)
        .await;
    assert!(matches!(started, Decision::Continue));
    assert_eq!(ind.starts.load(Ordering::SeqCst), 1);
    let completed = hook
        .on_event(
            &Event::ToolCallCompleted {
                call,
                result: ToolResult::success(json!("ok")),
            },
            &rc,
        )
        .await;
    assert!(matches!(completed, Decision::Continue));
    assert_eq!(ind.chunks.load(Ordering::SeqCst), 4);

    hook.on_stop(&rc, &Outcome::Errored("boom".into())).await;
    assert_eq!(ind.stops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ordered_delivery_under_burst() {
    let ind: Arc<CapturingIndicator> = Arc::new(CapturingIndicator::default());
    let hook = SlackAgentProgressHook::new(ind.clone());
    let rc = slack_ctx();

    for index in 0..10 {
        let call = ToolCall {
            id: format!("call-{index}"),
            name: format!("tool-{index}"),
            args: json!({}),
            thought_signature: None,
        };
        let _ = hook
            .on_event(&Event::ToolCallStarted(call.clone()), &rc)
            .await;
        let _ = hook
            .on_event(
                &Event::ToolCallCompleted {
                    call,
                    result: ToolResult::success(json!("ok")),
                },
                &rc,
            )
            .await;
    }

    let chunks = ind.chunks().await;
    assert_eq!(chunks.len(), 40);
    for index in 0..10 {
        let base = index * 4;
        let id = format!("call-{index}");
        let name = format!("tool-{index}");
        assert!(
            matches!(&chunks[base], ProgressChunk::Status(status) if status == &format!("calling {name}"))
        );
        let ProgressChunk::TaskUpdate(started) = &chunks[base + 1] else {
            panic!("expected started task_update");
        };
        assert_eq!(started.id, id);
        assert_eq!(started.title, name);
        assert_eq!(started.status, TaskStatus::InProgress);
        assert!(
            matches!(&chunks[base + 2], ProgressChunk::Status(status) if status == &format!("{name} done"))
        );
        let ProgressChunk::TaskUpdate(completed) = &chunks[base + 3] else {
            panic!("expected completed task_update");
        };
        assert_eq!(completed.id, format!("call-{index}"));
        assert_eq!(completed.title, format!("tool-{index}"));
        assert_eq!(completed.status, TaskStatus::Complete);
    }
}

#[test]
fn hook_debug_omits_indicator_state() {
    let hook = SlackAgentProgressHook::new(Arc::new(NoopSlackAgentProgress));
    let rendered = format!("{hook:?}");
    assert!(
        rendered.starts_with("SlackAgentProgressHook"),
        "unexpected debug: {rendered}"
    );
    assert!(
        rendered.contains(".."),
        "debug must use finish_non_exhaustive"
    );
}

#[tokio::test]
async fn hook_ignores_non_tool_call_events() {
    let ind: Arc<CapturingIndicator> = Arc::new(CapturingIndicator::default());
    let hook = SlackAgentProgressHook::new(ind.clone());
    let rc = slack_ctx();

    let token = hook.on_event(&Event::Token("hi".into()), &rc).await;
    assert!(matches!(token, Decision::Continue));
    let final_ev = hook.on_event(&Event::Final("done".into()), &rc).await;
    assert!(matches!(final_ev, Decision::Continue));
    assert!(ind.chunks().await.is_empty());
}

#[tokio::test]
async fn every_outcome_triggers_stop_when_started() {
    let ind: Arc<CapturingIndicator> = Arc::new(CapturingIndicator::default());
    let hook = SlackAgentProgressHook::new(ind.clone());
    let rc = slack_ctx();

    let outcomes = [
        Outcome::Completed("ok".into()),
        Outcome::MaxTurnsExceeded,
        Outcome::Cancelled,
        Outcome::Errored("nope".into()),
    ];
    for outcome in &outcomes {
        let call = ToolCall {
            id: "1".into(),
            name: "calendar".into(),
            args: json!({}),
            thought_signature: None,
        };
        let _ = hook.on_event(&Event::ToolCallStarted(call), &rc).await;
        hook.on_stop(&rc, outcome).await;
    }
    assert_eq!(ind.stops.load(Ordering::SeqCst), outcomes.len());
}
