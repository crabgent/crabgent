//! Slack agent-progress trait surface and supporting types.
//!
//! `SlackAgentProgress` is the Slack-specific cousin of
//! `crabgent_thinking::TypingIndicator`: it exposes a richer progress
//! surface so callers can drive `assistant.threads.setStatus` (V2) and
//! the `chat.startStream` / `chat.appendStream` / `chat.stopStream`
//! triad (V3) on the same lifecycle hooks. The existing
//! `SlackTypingIndicator` no-op stays in place as the V1 surface.

use std::time::Duration;

use async_trait::async_trait;
use crabgent_core::RunCtx;
use thiserror::Error;

use crate::block_kit::{BlocksChunk, MarkdownTextChunk, PlanUpdateChunk, TaskUpdateChunk};
use crate::error::SlackError;

/// Heartbeat cadence for `assistant.threads.setStatus`. Slack clears the
/// status after about 120s of silence; 90s keeps a 30s safety buffer.
pub const AGENT_STATUS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(90);

/// V3 consumer idle-flush window. Chunks accumulating in the buffer are
/// flushed via a single `chat.appendStream` after this much silence on
/// the inbound channel.
pub const DEFAULT_IDLE_FLUSH_INTERVAL: Duration = Duration::from_millis(250);

/// Per-indicator configuration for the Slack agent-progress surfaces.
///
/// The default enables both V2 (`assistant.threads.setStatus`) and V3
/// (`chat.*Stream`) with the production-tuned timings. Consumers that
/// want to disable either surface, swap silent-tool filtering, or speed
/// up the heartbeat for local iteration build a config explicitly via
/// [`AgentProgressConfig::default`] plus the `with_*` setters.
#[derive(Debug, Clone)]
pub struct AgentProgressConfig {
    /// Post `assistant.threads.setStatus` (V2 bubble) on `start`, on
    /// every chunk-status, and clear on `stop`. Heartbeat reposts the
    /// last status every `heartbeat_interval`.
    pub enable_bubble: bool,
    /// Open and stream the V3 thinking card via `chat.startStream`,
    /// `chat.appendStream`, `chat.stopStream`.
    pub enable_card: bool,
    /// Heartbeat cadence for V2. Ignored when `enable_bubble = false`.
    pub heartbeat_interval: Duration,
    /// Idle-flush window for the V3 consumer task. Ignored when
    /// `enable_card = false`.
    pub idle_flush_interval: Duration,
}

impl Default for AgentProgressConfig {
    fn default() -> Self {
        Self {
            enable_bubble: true,
            enable_card: true,
            heartbeat_interval: AGENT_STATUS_HEARTBEAT_INTERVAL,
            idle_flush_interval: DEFAULT_IDLE_FLUSH_INTERVAL,
        }
    }
}

/// Slack error codes that classify the workspace/bot as non-agent.
///
/// When `assistant.threads.setStatus` or `chat.startStream` returns one
/// of these codes the auto-detect path transitions from `Unknown` to
/// `Standard`, logs a single `warn!`, and stops attempting agent surfaces
/// for the rest of the indicator's lifetime.
pub const SENTINEL_NOT_AGENT_ERRORS: &[&str] = &[
    "feature_not_supported",
    "not_allowed_token_type",
    "access_denied",
];

/// Errors surfaced by `SlackAgentProgress` implementations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AgentProgressError {
    /// Transport-layer failure. The string is redaction-safe: it never
    /// contains token material because `SlackError`'s `Display` impl is
    /// likewise redaction-safe.
    #[error("agent progress transport failed: {0}")]
    Transport(String),
}

/// Result alias for agent-progress operations.
pub type AgentProgressResult<T> = Result<T, AgentProgressError>;

impl From<SlackError> for AgentProgressError {
    fn from(error: SlackError) -> Self {
        Self::Transport(format!("{error}"))
    }
}

/// Progress signal a `SlackAgentProgress` impl can deliver.
///
/// `Status` is the simple "thinking" string surfaced via
/// `assistant.threads.setStatus`. The remaining variants are forwarded
/// into an active stream via `chat.appendStream`.
#[derive(Debug, Clone)]
pub enum ProgressChunk {
    /// Plain status label for V2 (`assistant.threads.setStatus`).
    Status(String),
    /// Inline markdown stream chunk.
    MarkdownText(MarkdownTextChunk),
    /// Task progress envelope.
    TaskUpdate(TaskUpdateChunk),
    /// Plan-level update envelope.
    PlanUpdate(PlanUpdateChunk),
    /// Raw Block Kit blocks envelope.
    Blocks(BlocksChunk),
}

/// Workspace/bot classification used by the auto-detect path.
///
/// `Unknown` is the cold-start default; the first agent-surface call
/// transitions to `AiAgent` (on success) or `Standard` (on sentinel
/// error). `repr(u8)` lets the indicator publish state via
/// `AtomicU8::store` / `AtomicU8::load` without taking a lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SlackAppType {
    /// Auto-detect has not run.
    Unknown = 0,
    /// Workspace accepted an agent-surface call.
    AiAgent = 1,
    /// Workspace returned a sentinel non-agent error.
    Standard = 2,
}

impl SlackAppType {
    /// Project the enum value to its underlying `u8`.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for SlackAppType {
    type Error = AgentProgressError;

    fn try_from(value: u8) -> AgentProgressResult<Self> {
        match value {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::AiAgent),
            2 => Ok(Self::Standard),
            other => Err(AgentProgressError::Transport(format!(
                "invalid SlackAppType u8: {other}"
            ))),
        }
    }
}

/// Slack-specific agent-progress surface.
///
/// `start` is called once per run (from `on_session_start`).
/// `chunk` is called for each progress event.
/// `stop` is called once per run (from `on_stop`, for every `Outcome`).
///
/// Implementations short-circuit when `ctx.subject` does not target a
/// Slack run.
#[async_trait]
pub trait SlackAgentProgress: Send + Sync {
    /// Open the progress surface for the run.
    async fn start(&self, ctx: &RunCtx, initial_status: &str) -> AgentProgressResult<()>;

    /// Deliver a single progress chunk for the run.
    async fn chunk(&self, ctx: &RunCtx, chunk: ProgressChunk) -> AgentProgressResult<()>;

    /// Close the progress surface for the run.
    async fn stop(&self, ctx: &RunCtx) -> AgentProgressResult<()>;
}

/// `SlackAgentProgress` implementation that accepts and discards every
/// call. Useful as the default for builders that do not wire a live
/// indicator yet.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSlackAgentProgress;

#[async_trait]
impl SlackAgentProgress for NoopSlackAgentProgress {
    async fn start(&self, _ctx: &RunCtx, _initial_status: &str) -> AgentProgressResult<()> {
        Ok(())
    }

    async fn chunk(&self, _ctx: &RunCtx, _chunk: ProgressChunk) -> AgentProgressResult<()> {
        Ok(())
    }

    async fn stop(&self, _ctx: &RunCtx) -> AgentProgressResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::{RunId, Subject};

    fn ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("agent-progress-user"))
    }

    #[test]
    fn slack_app_type_round_trips_via_u8() {
        for variant in [
            SlackAppType::Unknown,
            SlackAppType::AiAgent,
            SlackAppType::Standard,
        ] {
            let raw = variant.to_u8();
            let back = SlackAppType::try_from(raw).expect("known variant");
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn slack_app_type_rejects_invalid_u8() {
        let err = SlackAppType::try_from(255).expect_err("invalid value rejected");
        match err {
            AgentProgressError::Transport(msg) => {
                assert!(
                    msg.contains("invalid SlackAppType u8"),
                    "unexpected message: {msg}"
                );
            }
        }
    }

    #[test]
    fn sentinel_list_carries_known_codes() {
        for code in [
            "feature_not_supported",
            "not_allowed_token_type",
            "access_denied",
        ] {
            assert!(
                SENTINEL_NOT_AGENT_ERRORS.contains(&code),
                "missing sentinel: {code}"
            );
        }
    }

    #[test]
    fn heartbeat_interval_is_ninety_seconds() {
        assert_eq!(AGENT_STATUS_HEARTBEAT_INTERVAL, Duration::from_secs(90));
    }

    #[tokio::test]
    async fn noop_indicator_accepts_every_call() {
        let ind = NoopSlackAgentProgress;
        let c = ctx();
        ind.start(&c, "thinking...").await.expect("start ok");
        ind.chunk(&c, ProgressChunk::Status("step".into()))
            .await
            .expect("chunk ok");
        ind.stop(&c).await.expect("stop ok");
    }

    #[test]
    fn slack_error_conversion_is_redaction_safe() {
        let err = SlackError::ApiError {
            slack_code: "feature_not_supported".to_owned(),
            http_status: Some(200),
        };
        let mapped: AgentProgressError = err.into();
        let msg = format!("{mapped}");
        assert!(!msg.contains("xoxb"), "token leak: {msg}");
        assert!(!msg.contains("xapp"), "token leak: {msg}");
        assert!(msg.contains("feature_not_supported"), "missing code: {msg}");
    }
}
