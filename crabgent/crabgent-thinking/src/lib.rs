//! Generic progress-step sinks for channel adapters and host runtimes.
//!
//! The crate intentionally has no channel dependency. Adapters can push
//! concise progress steps to any `ThinkingChannel`, while hosts decide how
//! those steps are rendered or buffered.
//!
//! Beyond progress steps, the crate exposes a `TypingIndicator` surface
//! (see [`typing`]) that channel adapters can implement to surface a
//! "agent is working" signal alongside the regular reply.

mod typing;

pub use typing::{NoopTypingIndicator, TypingError, TypingHook, TypingIndicator, TypingResult};

use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

use async_trait::async_trait;
use crabgent_core::{Decision, Event, Hook, Notification, RunCtx};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;

/// One user-visible progress step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingStep {
    /// Short label suitable for compact rendering.
    pub label: String,
    /// Optional detail string. Avoid user payloads and secrets.
    pub detail: Option<String>,
    /// Unix timestamp in milliseconds.
    pub ts: u64,
}

impl ThinkingStep {
    /// Build a step with the current wall-clock timestamp.
    pub fn now(label: impl Into<String>, detail: Option<impl Into<String>>) -> Result<Self> {
        Ok(Self {
            label: label.into(),
            detail: detail.map(Into::into),
            ts: unix_millis()?,
        })
    }

    /// Build a step with an explicit timestamp.
    #[must_use]
    pub fn at(label: impl Into<String>, detail: Option<impl Into<String>>, ts: u64) -> Self {
        Self {
            label: label.into(),
            detail: detail.map(Into::into),
            ts,
        }
    }
}

/// Error type for progress sinks.
#[derive(Debug, Error)]
pub enum ThinkingError {
    /// The system clock was earlier than the Unix epoch.
    #[error("system clock before unix epoch")]
    Clock(#[from] SystemTimeError),
}

/// Crate-local result alias.
pub type Result<T> = std::result::Result<T, ThinkingError>;

/// Destination for progress steps.
#[async_trait]
pub trait ThinkingChannel: Send + Sync {
    /// Push one progress step for the given run.
    ///
    /// `ctx` carries `RunId` and `Subject`, letting a single global sink
    /// multiplex progress steps per kernel run (e.g. one live-edited
    /// "thinking" bubble per Slack thread).
    async fn push_step(&self, step: ThinkingStep, ctx: &RunCtx) -> Result<()>;
}

/// Renderer for collected progress state.
#[async_trait]
pub trait ProgressSink: ThinkingChannel {
    /// Render current progress state.
    async fn render(&self) -> Result<String>;
}

/// Sink that accepts and discards all progress steps.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSink;

#[async_trait]
impl ThinkingChannel for NoopSink {
    async fn push_step(&self, _step: ThinkingStep, _ctx: &RunCtx) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl ProgressSink for NoopSink {
    async fn render(&self) -> Result<String> {
        Ok(String::new())
    }
}

/// In-memory sink for tests and simple hosts.
#[derive(Debug, Default)]
pub struct BufferingSink {
    steps: Mutex<Vec<ThinkingStep>>,
}

impl BufferingSink {
    /// Build an empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a snapshot of buffered steps.
    pub async fn steps(&self) -> Vec<ThinkingStep> {
        self.steps.lock().await.clone()
    }

    /// Remove all buffered steps.
    pub async fn clear(&self) {
        self.steps.lock().await.clear();
    }
}

#[async_trait]
impl ThinkingChannel for BufferingSink {
    async fn push_step(&self, step: ThinkingStep, _ctx: &RunCtx) -> Result<()> {
        self.steps.lock().await.push(step);
        Ok(())
    }
}

#[async_trait]
impl ProgressSink for BufferingSink {
    async fn render(&self) -> Result<String> {
        let steps = self.steps.lock().await;
        Ok(render_steps(&steps))
    }
}

/// Hook bridge that translates safe kernel lifecycle events into steps.
pub struct ThinkingHook {
    sink: Arc<dyn ThinkingChannel>,
}

impl fmt::Debug for ThinkingHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThinkingHook").finish_non_exhaustive()
    }
}

impl ThinkingHook {
    /// Build a hook bridge for the given sink.
    #[must_use]
    pub fn new(sink: Arc<dyn ThinkingChannel>) -> Self {
        Self { sink }
    }

    async fn push(&self, ctx: &RunCtx, label: &str, detail: Option<String>) {
        if let Ok(step) = ThinkingStep::now(label, detail)
            && let Err(err) = self.sink.push_step(step, ctx).await
        {
            crabgent_log::debug!(error = %err, "thinking step sink rejected progress update");
        }
    }
}

#[async_trait]
impl Hook for ThinkingHook {
    async fn on_event(&self, ev: &Event, ctx: &RunCtx) -> Decision<Event> {
        match ev {
            Event::ToolCallStarted(call) => {
                self.push(ctx, "tool_started", Some(call.name.clone()))
                    .await;
            }
            Event::ToolCallCompleted { call, .. } => {
                self.push(ctx, "tool_completed", Some(call.name.clone()))
                    .await;
            }
            Event::Reasoning(detail) => {
                self.push(ctx, "reasoning", Some(detail.clone())).await;
            }
            _ => {}
        }
        Decision::Continue
    }

    async fn on_notification(&self, note: &Notification, ctx: &RunCtx) -> Decision<()> {
        self.push_notification(ctx, note).await;
        Decision::Continue
    }
}

impl ThinkingHook {
    async fn push_notification(&self, ctx: &RunCtx, note: &Notification) {
        self.push(ctx, "notification", Some(note.kind.clone()))
            .await;
    }
}

fn render_steps(steps: &[ThinkingStep]) -> String {
    steps.iter().map(render_step).collect::<Vec<_>>().join("\n")
}

fn render_step(step: &ThinkingStep) -> String {
    match step.detail.as_deref() {
        Some(detail) => format!("{}: {}", step.label, detail),
        None => step.label.clone(),
    }
}

fn unix_millis() -> Result<u64> {
    let duration = SystemTime::now().duration_since(UNIX_EPOCH)?;
    Ok(duration.as_millis().try_into().unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::{NotificationLevel, RunId, Subject, ToolCall, ToolResult};

    fn ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("test-user"))
    }

    fn tool_call(name: &str) -> ToolCall {
        ToolCall {
            id: "tool-call-1".into(),
            name: name.into(),
            #[expect(
                clippy::default_trait_access,
                reason = "serde_json::Value is intentionally not a direct crabgent-thinking dependency"
            )]
            args: Default::default(),
            thought_signature: None,
        }
    }

    fn notification(kind: &str) -> Notification {
        Notification {
            kind: kind.into(),
            message: "message".into(),
            level: NotificationLevel::Info,
        }
    }

    #[tokio::test]
    async fn noop_sink_discards_steps() {
        let sink = NoopSink;
        sink.push_step(ThinkingStep::at("start", None::<String>, 1), &ctx())
            .await
            .expect("push");

        assert_eq!(sink.render().await.expect("render"), "");
    }

    #[tokio::test]
    async fn buffering_sink_keeps_and_renders_steps() {
        let sink = BufferingSink::new();
        let c = ctx();
        sink.push_step(ThinkingStep::at("start", Some("one"), 1), &c)
            .await
            .expect("push");
        sink.push_step(ThinkingStep::at("done", None::<String>, 2), &c)
            .await
            .expect("push");

        let steps = sink.steps().await;
        assert_eq!(steps.len(), 2);
        assert_eq!(sink.render().await.expect("render"), "start: one\ndone");
    }

    #[tokio::test]
    async fn trait_object_accepts_steps() {
        let sink: Arc<dyn ProgressSink> = Arc::new(BufferingSink::new());
        sink.push_step(ThinkingStep::at("queued", Some("tool"), 1), &ctx())
            .await
            .expect("push");

        assert_eq!(sink.render().await.expect("render"), "queued: tool");
    }

    #[tokio::test]
    async fn thinking_hook_records_tool_lifecycle_events() {
        let sink = Arc::new(BufferingSink::new());
        let hook = ThinkingHook::new(sink.clone());
        let ctx = ctx();
        let call = tool_call("bash");

        assert!(matches!(
            hook.on_event(&Event::ToolCallStarted(call.clone()), &ctx)
                .await,
            Decision::Continue
        ));
        assert!(matches!(
            hook.on_event(
                &Event::ToolCallCompleted {
                    call: call.clone(),
                    result: ToolResult {
                        call_id: "c1".into(),
                        #[expect(
                            clippy::default_trait_access,
                            reason = "serde_json::Value is intentionally not a direct crabgent-thinking dependency"
                        )]
                        output: Default::default(),
                        is_error: false,
                        run_messages: Vec::new(),
                    },
                },
                &ctx
            )
            .await,
            Decision::Continue
        ));

        let steps = sink.steps().await;
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].label, "tool_started");
        assert_eq!(steps[0].detail.as_deref(), Some("bash"));
        assert_eq!(steps[1].label, "tool_completed");
        assert_eq!(steps[1].detail.as_deref(), Some("bash"));
        assert_eq!(
            sink.render().await.expect("render"),
            "tool_started: bash\ntool_completed: bash"
        );
    }

    #[derive(Default)]
    struct CapturingSink {
        steps: Mutex<Vec<(RunId, ThinkingStep)>>,
    }

    #[async_trait]
    impl ThinkingChannel for CapturingSink {
        async fn push_step(&self, step: ThinkingStep, ctx: &RunCtx) -> Result<()> {
            self.steps.lock().await.push((ctx.run_id.clone(), step));
            Ok(())
        }
    }

    #[tokio::test]
    async fn thinking_hook_threads_run_id_to_sink() {
        let sink = Arc::new(CapturingSink::default());
        let hook = ThinkingHook::new(sink.clone());
        let ctx_a = ctx();
        let ctx_b = ctx();

        hook.on_event(&Event::ToolCallStarted(tool_call("bash")), &ctx_a)
            .await;
        hook.on_event(&Event::ToolCallStarted(tool_call("bash")), &ctx_b)
            .await;

        let captured = sink.steps.lock().await;
        assert_eq!(captured.len(), 2);
        assert_eq!(&captured[0].0, &ctx_a.run_id);
        assert_eq!(&captured[1].0, &ctx_b.run_id);
        assert_ne!(&captured[0].0, &captured[1].0);
    }

    #[tokio::test]
    async fn thinking_hook_records_reasoning_events() {
        let sink = Arc::new(BufferingSink::new());
        let hook = ThinkingHook::new(sink.clone());
        let ctx = ctx();

        assert!(matches!(
            hook.on_event(&Event::Reasoning("chain-of-thought".into()), &ctx)
                .await,
            Decision::Continue
        ));

        let steps = sink.steps().await;
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].label, "reasoning");
        assert_eq!(steps[0].detail.as_deref(), Some("chain-of-thought"));
    }

    #[tokio::test]
    async fn thinking_hook_buffers_notifications_only_from_notification_surface() {
        let sink = Arc::new(BufferingSink::new());
        let hook = ThinkingHook::new(sink.clone());
        let ctx = ctx();
        let note = notification("analysis");

        assert!(matches!(
            hook.on_event(&Event::Notification(note.clone()), &ctx)
                .await,
            Decision::Continue
        ));
        assert!(matches!(
            hook.on_notification(&note, &ctx).await,
            Decision::Continue
        ));

        let steps = sink.steps().await;
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].label, "notification");
        assert_eq!(steps[0].detail.as_deref(), Some("analysis"));
        assert_eq!(
            sink.render().await.expect("render"),
            "notification: analysis"
        );
    }
}
