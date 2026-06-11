//! `SlackAgentProgressHook` bridge.
//!
//! Drives a `SlackAgentProgress` implementation from kernel lifecycle
//! events. The hook is lazy: `on_session_start` does not open any
//! surface, and `indicator.start` is deferred until the first non-silent
//! tool call. Tools that produce user-visible channel output (today:
//! `channel_send`) are never surfaced as thinking-card task entries, so
//! a plain question/answer run shows no card at all and the bubble only
//! flickers when the model actually does background work the user would
//! want to see.
//!
//! Indicator errors are fail-open: the hook logs them via
//! `crabgent_log::warn!` and returns `Decision::Continue`, mirroring
//! `crabgent_thinking::TypingHook`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use crabgent_core::{Decision, Event, Hook, Outcome, RunCtx, RunId};

use super::types::{ProgressChunk, SlackAgentProgress};
use crate::block_kit::{TaskStatus, TaskUpdateChunk};

/// Tool names that produce user-visible channel output and therefore must
/// not appear as thinking-card task entries. `channel_send` posts the bot
/// reply via `chat.postMessage`; surfacing it as a task chunk doubles the
/// visible response with no extra signal for the user. Consumers can
/// override this list per hook via [`SlackAgentProgressHook::with_silent_tools`].
pub const DEFAULT_SILENT_TOOLS: &[&str] = &["channel_send"];

/// Hook that bridges a `SlackAgentProgress` impl into the kernel run
/// lifecycle. Wire one hook per kernel via `KernelBuilder::with_hook`.
pub struct SlackAgentProgressHook {
    indicator: Arc<dyn SlackAgentProgress>,
    started: Arc<Mutex<HashSet<RunId>>>,
    silent_tools: HashSet<String>,
}

impl std::fmt::Debug for SlackAgentProgressHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackAgentProgressHook")
            .finish_non_exhaustive()
    }
}

impl SlackAgentProgressHook {
    /// Build a bridge with the default silent-tools list
    /// ([`DEFAULT_SILENT_TOOLS`]).
    #[must_use]
    pub fn new(indicator: Arc<dyn SlackAgentProgress>) -> Self {
        Self::with_silent_tools(
            indicator,
            DEFAULT_SILENT_TOOLS.iter().map(|s| (*s).to_owned()),
        )
    }

    /// Build a bridge with an explicit silent-tools iterator. Use this
    /// when the consumer wants to add (or replace) the channel-output
    /// tools that should never show up as thinking-card task entries.
    #[must_use]
    pub fn with_silent_tools(
        indicator: Arc<dyn SlackAgentProgress>,
        silent_tools: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            indicator,
            started: Arc::new(Mutex::new(HashSet::new())),
            silent_tools: silent_tools.into_iter().collect(),
        }
    }

    fn is_silent_tool(&self, name: &str) -> bool {
        self.silent_tools.contains(name)
    }

    fn lock_started(&self) -> MutexGuard<'_, HashSet<RunId>> {
        self.started.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn ensure_started(&self, ctx: &RunCtx) {
        let inserted = self.lock_started().insert(ctx.run_id.clone());
        if !inserted {
            return;
        }
        if let Err(err) = self.indicator.start(ctx, "thinking...").await {
            crabgent_log::warn!(
                run_id = %ctx.run_id,
                subject_id = %ctx.subject.id(),
                error = %err,
                "slack agent-progress start failed",
            );
        }
    }

    async fn forward_chunk(&self, ctx: &RunCtx, chunk: ProgressChunk) {
        if let Err(err) = self.indicator.chunk(ctx, chunk).await {
            crabgent_log::warn!(
                run_id = %ctx.run_id,
                subject_id = %ctx.subject.id(),
                error = %err,
                "slack agent-progress chunk failed",
            );
        }
    }
}

#[async_trait]
impl Hook for SlackAgentProgressHook {
    async fn on_session_start(&self, _ctx: &RunCtx) -> Decision<()> {
        // Lazy: the surface only opens once a non-silent tool actually fires.
        Decision::Continue
    }

    async fn on_event(&self, ev: &Event, ctx: &RunCtx) -> Decision<Event> {
        match ev {
            Event::ToolCallStarted(call) if !self.is_silent_tool(&call.name) => {
                self.ensure_started(ctx).await;
                self.forward_chunk(ctx, ProgressChunk::Status(format!("calling {}", call.name)))
                    .await;
                self.forward_chunk(
                    ctx,
                    ProgressChunk::TaskUpdate(TaskUpdateChunk {
                        id: call.id.clone(),
                        title: call.name.clone(),
                        status: TaskStatus::InProgress,
                        details: None,
                        output: None,
                        sources: None,
                    }),
                )
                .await;
            }
            Event::ToolCallCompleted { call, result } if !self.is_silent_tool(&call.name) => {
                self.ensure_started(ctx).await;
                let (label, status) = if result.is_error {
                    (format!("{} failed", call.name), TaskStatus::Error)
                } else {
                    (format!("{} done", call.name), TaskStatus::Complete)
                };
                self.forward_chunk(ctx, ProgressChunk::Status(label)).await;
                self.forward_chunk(
                    ctx,
                    ProgressChunk::TaskUpdate(TaskUpdateChunk {
                        id: call.id.clone(),
                        title: call.name.clone(),
                        status,
                        details: None,
                        output: None,
                        sources: None,
                    }),
                )
                .await;
            }
            _ => {}
        }
        Decision::Continue
    }

    async fn on_stop(&self, ctx: &RunCtx, _outcome: &Outcome) {
        let was_started = self.lock_started().remove(&ctx.run_id);
        if !was_started {
            return;
        }
        if let Err(err) = self.indicator.stop(ctx).await {
            crabgent_log::warn!(
                run_id = %ctx.run_id,
                subject_id = %ctx.subject.id(),
                error = %err,
                "slack agent-progress stop failed",
            );
        }
    }
}
