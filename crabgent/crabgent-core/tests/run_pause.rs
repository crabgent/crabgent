//! Cooperative pause: safe-boundary checks, `Outcome::Paused`, shutdown
//! attribution, and `Kernel::shutdown_with_pause`.

// Shared doubles live in `lifecycle_support`; not every helper is used by this
// binary, so unused-item warnings on the shared module are expected here.
#![allow(dead_code, reason = "shared lifecycle_support has unused helpers")]

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    AllowAllPolicy, CancelReason, Event, Hook, Kernel, KernelError, LlmRequest, LlmResponse,
    Message, Outcome, RunCtx, Tool, ToolCall, ToolCtx, ToolError, ToolResult,
};
use crabgent_test_support::{tool_call, tool_use};

mod lifecycle_support;

use lifecycle_support::{
    CaptureMessageHook, NoopTool, PendingProvider, TraceHook, calling_tool, done, request,
    scripted, trace_snapshot, wait_for_trace,
};

/// Tool that sleeps briefly so pause requests fired mid-turn land before
/// the run reaches its next safe boundary.
struct SlowTool;

#[async_trait]
impl Tool for SlowTool {
    fn name(&self) -> &'static str {
        "slow"
    }

    fn description(&self) -> &'static str {
        "test tool that sleeps briefly"
    }

    fn parameters_schema(&self) -> Value {
        json!({})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(json!({"ok": true}))
    }
}

fn slow_call() -> LlmResponse {
    tool_use(vec![tool_call("c1", "slow", json!({}))])
}

/// Fires the given pause token from `after_llm`, i.e. while the current
/// turn is still being applied.
struct PauseAfterLlmHook {
    pause: CancellationToken,
}

#[async_trait]
impl Hook for PauseAfterLlmHook {
    async fn after_llm(
        &self,
        _req: &LlmRequest,
        _resp: &LlmResponse,
        _ctx: &RunCtx,
    ) -> crabgent_core::Decision<LlmResponse> {
        self.pause.cancel();
        crabgent_core::Decision::Continue
    }
}

/// Fires the given pause token from `after_tool`, i.e. after the turn's
/// tool finished but before the next turn starts.
struct PauseAfterToolHook {
    pause: CancellationToken,
}

#[async_trait]
impl Hook for PauseAfterToolHook {
    async fn after_tool(
        &self,
        _call: &ToolCall,
        _result: &ToolResult,
        _ctx: &RunCtx,
    ) -> crabgent_core::Decision<ToolResult> {
        self.pause.cancel();
        crabgent_core::Decision::Continue
    }
}

/// Captures `ctx.cancel_reason()` as observed at `on_stop` time.
struct ReasonCaptureHook {
    seen: Arc<Mutex<Option<CancelReason>>>,
}

#[async_trait]
impl Hook for ReasonCaptureHook {
    async fn on_stop(&self, ctx: &RunCtx, _outcome: &Outcome) {
        *self.seen.lock().expect("mutex should not be poisoned") = ctx.cancel_reason();
    }
}

fn paused_request(max_turns: u32, pause: &CancellationToken) -> crabgent_core::RunRequest {
    let mut req = request(max_turns);
    req.pause = Some(pause.clone());
    req
}

#[tokio::test]
async fn pre_fired_pause_token_pauses_before_first_turn() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let pause = CancellationToken::new();
    pause.cancel();
    let kernel = Kernel::builder()
        .provider(scripted(vec![done("never")]))
        .policy(AllowAllPolicy)
        .add_hook(TraceHook::new(trace.clone()))
        .build();

    let stream = kernel.run_streaming(paused_request(5, &pause), None);
    tokio::pin!(stream);
    let item = stream.next().await.expect("one terminal item");

    assert!(matches!(item, Err(KernelError::Paused)));
    let events = trace_snapshot(&trace);
    assert!(events.contains(&"on_stop:paused".to_string()), "{events:?}");
    assert!(
        !events.contains(&"before_llm".to_string()),
        "no provider turn must start after a pre-fired pause: {events:?}"
    );
}

#[tokio::test]
async fn pause_after_tool_stops_at_next_turn_boundary() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let pause = CancellationToken::new();
    let kernel = Kernel::builder()
        .provider(scripted(vec![calling_tool(), done("never")]))
        .policy(AllowAllPolicy)
        .add_tool(NoopTool)
        .add_hook(TraceHook::new(trace.clone()))
        .add_hook(PauseAfterToolHook {
            pause: pause.clone(),
        })
        .build();

    let stream = kernel.run_streaming(paused_request(5, &pause), None);
    tokio::pin!(stream);
    let mut terminal = None;
    while let Some(item) = stream.next().await {
        if item.is_err() {
            terminal = Some(item);
            break;
        }
    }

    assert!(matches!(terminal, Some(Err(KernelError::Paused))));
    let events = trace_snapshot(&trace);
    let llm_turns = events.iter().filter(|e| *e == "before_llm").count();
    assert_eq!(llm_turns, 1, "pause lands before turn two: {events:?}");
    assert!(
        events.contains(&"event:tool_completed".to_string()),
        "turn one's tool completed before the pause: {events:?}"
    );
    assert!(events.contains(&"on_stop:paused".to_string()), "{events:?}");
}

#[tokio::test]
async fn pause_after_llm_stops_before_tool_dispatch_leaving_dangling_call() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let pause = CancellationToken::new();
    let kernel = Kernel::builder()
        .provider(scripted(vec![calling_tool(), done("never")]))
        .policy(AllowAllPolicy)
        .add_tool(NoopTool)
        .add_hook(CaptureMessageHook::new(seen.clone()))
        .add_hook(PauseAfterLlmHook {
            pause: pause.clone(),
        })
        .build();

    let err = kernel
        .run(paused_request(5, &pause), None)
        .await
        .expect_err("paused run");

    assert!(matches!(err, KernelError::Paused));
    let messages = seen.lock().expect("mutex should not be poisoned").clone();
    match messages.last() {
        Some(Message::Assistant { tool_calls, .. }) => {
            assert_eq!(tool_calls.len(), 1, "dangling tool call stays in the log");
        }
        other => panic!("expected dangling assistant tool call, got {other:?}"),
    }
}

#[tokio::test]
async fn pause_request_never_downgrades_a_completed_run() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let pause = CancellationToken::new();
    let kernel = Kernel::builder()
        .provider(scripted(vec![done("finished")]))
        .policy(AllowAllPolicy)
        .add_hook(TraceHook::new(trace.clone()))
        .add_hook(PauseAfterLlmHook {
            pause: pause.clone(),
        })
        .build();

    let text = kernel
        .run(paused_request(5, &pause), None)
        .await
        .expect("completed run wins over pending pause");

    assert_eq!(text, "finished");
    assert!(
        trace_snapshot(&trace).contains(&"on_stop:completed:finished".to_string()),
        "{:?}",
        trace_snapshot(&trace)
    );
}

#[tokio::test]
async fn request_pause_reaches_runs_without_request_plumbing() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let responses: Vec<LlmResponse> = (0..40).map(|_| slow_call()).collect();
    let kernel = Arc::new(
        Kernel::builder()
            .provider(scripted(responses))
            .policy(AllowAllPolicy)
            .add_tool(SlowTool)
            .add_hook(TraceHook::new(trace.clone()))
            .build(),
    );

    let stream = kernel.run_streaming(request(50), None);
    tokio::pin!(stream);
    let mut terminal = None;
    while let Some(item) = stream.next().await {
        if matches!(item, Ok(Event::ToolCallStarted(_))) {
            kernel.request_pause();
        }
        if item.is_err() {
            terminal = Some(item);
            break;
        }
    }

    assert!(matches!(terminal, Some(Err(KernelError::Paused))));
    assert!(
        trace_snapshot(&trace).contains(&"on_stop:paused".to_string()),
        "{:?}",
        trace_snapshot(&trace)
    );
}

#[tokio::test]
async fn kernel_shutdown_stamps_shutdown_cancel_reason() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::new(Mutex::new(None));
    let kernel = Arc::new(
        Kernel::builder()
            .provider(PendingProvider)
            .policy(AllowAllPolicy)
            .add_hook(TraceHook::new(trace.clone()))
            .add_hook(ReasonCaptureHook { seen: seen.clone() })
            .with_graceful_shutdown(Duration::from_secs(2))
            .build(),
    );

    let stream = Box::pin(kernel.run_streaming(request(5), None));
    wait_for_trace(&trace, "before_llm").await;
    kernel.shutdown().await;
    drop(stream);

    assert!(trace_snapshot(&trace).contains(&"on_stop:cancelled".to_string()));
    assert_eq!(
        *seen.lock().expect("mutex should not be poisoned"),
        Some(CancelReason::Shutdown)
    );
}

#[tokio::test]
async fn stop_pattern_attribution_wins_over_shutdown_stamp() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::new(Mutex::new(None));
    let cell: Arc<OnceLock<CancelReason>> = Arc::new(OnceLock::new());
    cell.set(CancelReason::StopPattern)
        .expect("fresh cell accepts first write");
    let kernel = Arc::new(
        Kernel::builder()
            .provider(PendingProvider)
            .policy(AllowAllPolicy)
            .add_hook(TraceHook::new(trace.clone()))
            .add_hook(ReasonCaptureHook { seen: seen.clone() })
            .with_graceful_shutdown(Duration::from_secs(2))
            .build(),
    );

    let mut req = request(5);
    req.cancel_reason = Some(Arc::clone(&cell));
    let stream = Box::pin(kernel.run_streaming(req, None));
    wait_for_trace(&trace, "before_llm").await;
    kernel.shutdown().await;
    drop(stream);

    assert_eq!(
        *seen.lock().expect("mutex should not be poisoned"),
        Some(CancelReason::StopPattern),
        "user cancel intent beats the shutdown stamp"
    );
}

#[tokio::test]
async fn shutdown_with_pause_drains_cooperative_run_as_paused() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let responses: Vec<LlmResponse> = (0..40).map(|_| slow_call()).collect();
    let kernel = Arc::new(
        Kernel::builder()
            .provider(scripted(responses))
            .policy(AllowAllPolicy)
            .add_tool(SlowTool)
            .add_hook(TraceHook::new(trace.clone()))
            .with_graceful_shutdown(Duration::from_secs(2))
            .build(),
    );

    let stream = Box::pin(kernel.run_streaming(request(50), None));
    wait_for_trace(&trace, "event:tool_completed").await;
    kernel.shutdown_with_pause(Duration::from_secs(5)).await;

    let events = trace_snapshot(&trace);
    assert!(
        events.contains(&"on_stop:paused".to_string()),
        "cooperative run exits paused during the pause window: {events:?}"
    );
    assert!(kernel.shutdown_token().is_cancelled());
    drop(stream);
}

#[tokio::test]
async fn shutdown_with_pause_falls_back_to_cancel_for_stuck_run() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let kernel = Arc::new(
        Kernel::builder()
            .provider(PendingProvider)
            .policy(AllowAllPolicy)
            .add_hook(TraceHook::new(trace.clone()))
            .with_graceful_shutdown(Duration::from_secs(2))
            .build(),
    );

    let stream = Box::pin(kernel.run_streaming(request(5), None));
    wait_for_trace(&trace, "before_llm").await;
    let started = tokio::time::Instant::now();
    kernel.shutdown_with_pause(Duration::from_millis(150)).await;

    assert!(started.elapsed() >= Duration::from_millis(150));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        trace_snapshot(&trace).contains(&"on_stop:cancelled".to_string()),
        "a run that never reaches a safe boundary is cancelled, not paused: {:?}",
        trace_snapshot(&trace)
    );
    drop(stream);
}
