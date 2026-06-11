//! Hook trait, `Decision` enum, run-time context types, and `Event`.
//!
//! Chain semantics (`HookChain` in `hook_chain`): hooks run in
//! registration order. `Decision::Continue` leaves state unchanged.
//! `Decision::Replace(t)` substitutes state for the next hook.
//! `Decision::Deny(reason)` short-circuits and surfaces as
//! `KernelError::HookDenied`.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::task::JoinError;
use tokio_util::sync::CancellationToken;

use crate::error::KernelError;
use crate::message::Message;
use crate::model::{ModelId, ReasoningEffort};
use crate::run_id::RunId;
use crate::subject::Subject;
use crate::types::{LlmRequest, LlmResponse, Notification, ToolCall, ToolResult};

mod event;
pub use event::{AttemptErrorClass, Event};

/// Stable hook-chain decision.
///
/// Future variants are a breaking API change. If a future major version
/// adds a variant, every custom hook implementation that matches
/// `Decision` directly must be updated. Consumers implementing custom
/// hooks should pin `crabgent-core` to a compatible major version.
#[derive(Debug, Clone)]
pub enum Decision<T> {
    /// State unchanged, continue chain.
    Continue,
    /// Replace state, next hook sees the replacement.
    Replace(T),
    /// Short-circuit chain. Surfaces as `KernelError::HookDenied`.
    Deny(String),
}

/// Outcome of one `Kernel::run()` invocation, surfaced via `on_stop`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Outcome {
    Completed(String),
    MaxTurnsExceeded,
    Cancelled,
    /// The run stopped cooperatively at a safe boundary after a pause
    /// request. The message log at `on_stop` time is a clean resume
    /// point: persistence hooks should keep the full tail instead of
    /// trimming it the way `Cancelled` handling does.
    Paused,
    Errored(String),
}

/// Discriminator for `Outcome::Cancelled`: distinguishes who fired the
/// per-run `CancellationToken`.
///
/// Channel adapters set `CancelReason::StopPattern` from `cancel_conv`
/// before firing the token in response to a user-typed stop-pattern.
/// Application hooks set `CancelReason::Hook` from `before_llm`,
/// `after_llm`, or `after_tool` before calling `ctx.cancel.cancel()` to
/// gracefully stop the run after answer delivery. The kernel stamps
/// `CancelReason::Shutdown` itself when a run ends `Cancelled` with an
/// empty cell while the kernel-wide shutdown token is cancelled, so
/// `on_stop` observers can attribute shutdown-driven cancellation even
/// when the host wired no explicit pause plumbing. Executors that
/// force-cancel during a pause window set `Shutdown` before firing the
/// token. An unset cell at `on_stop` time defaults to
/// `CancelReason::External` from the observer's perspective
/// (kernel-internal drop or unattributed external cancel). The write
/// happens once via `Arc<OnceLock<_>>`; an earlier `StopPattern` write
/// wins over a later `Shutdown` stamp, so user cancel intent is
/// preserved across a shutdown race.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CancelReason {
    StopPattern,
    Hook,
    External,
    Shutdown,
}

/// Read-only context handed to every hook event.
///
/// `session_id` is a write-once shared cell populated by a
/// session-persisting hook (e.g. `crabgent-session::SessionPersistHook`)
/// during `on_session_start`, after the underlying store resolves the
/// `(Owner, ThreadId)` tuple to a concrete session id. Clones of the
/// `RunCtx` share the cell, so the kernel run-loop sees the value as
/// soon as the hook chain finishes resolving the session. The kernel
/// itself never sets it; it remains `None` when no session-persisting
/// hook is wired.
///
/// `session_model_override` and `session_reasoning_effort_override` follow
/// the same write-once shared pattern: a session-persisting hook publishes
/// persisted override values (if any) during `on_session_start`. The
/// streaming run-loop reads from these cells when resolving effective
/// runtime config, which lets the channel inbox stay decoupled from
/// `crabgent-store` while still honoring per-session overrides set via tools
/// like `models.set_session` and `models.set_session_effort`.
/// `RunRequest::session_model_override` (mirrored on `StreamCfg`) stays as a
/// low-level model escape hatch for callers without a session hook; the
/// hook-published value wins when both are present.
#[derive(Debug, Clone)]
pub struct RunCtx {
    pub run_id: RunId,
    pub subject: Subject,
    pub session_id: Arc<OnceLock<String>>,
    pub session_model_override: Arc<OnceLock<ModelId>>,
    pub session_reasoning_effort_override: Arc<OnceLock<ReasoningEffort>>,
    /// Per-run cancellation token shared with the kernel's `StreamCfg`
    /// and surfaced to hooks. Hooks may call `ctx.cancel.cancel()` from
    /// `before_llm`, `after_llm`, or `after_tool` to gracefully stop
    /// the run after answer delivery. The next provider call observes
    /// `is_cancelled()` and short-circuits to `Outcome::Cancelled`.
    /// `CancellationToken` is internally `Arc`-shared via clone, so the
    /// kernel keeps the same token when wiring `StreamCfg.cancel`.
    pub cancel: CancellationToken,
    /// Write-once discriminator for `Outcome::Cancelled`. Channel
    /// adapters set `CancelReason::StopPattern` from `cancel_conv`
    /// before firing the token. Application hooks set
    /// `CancelReason::Hook` before calling `cancel.cancel()`. An empty
    /// cell at `on_stop` time signals an external or kernel-internal
    /// drop without an explicit attribution. Clones share the cell.
    pub cancel_reason: Arc<OnceLock<CancelReason>>,
}

impl RunCtx {
    pub fn new(run_id: RunId, subject: Subject) -> Self {
        Self {
            run_id,
            subject,
            session_id: Arc::new(OnceLock::new()),
            session_model_override: Arc::new(OnceLock::new()),
            session_reasoning_effort_override: Arc::new(OnceLock::new()),
            cancel: CancellationToken::new(),
            cancel_reason: Arc::new(OnceLock::new()),
        }
    }

    /// Install a per-run `CancellationToken`. Intended for the kernel
    /// run-loop to thread the same child-token used by `StreamCfg.cancel`
    /// into `RunCtx`, so hooks and provider call-sites observe the same
    /// cancellation signal.
    #[must_use]
    pub fn with_cancel(mut self, token: CancellationToken) -> Self {
        self.cancel = token;
        self
    }

    /// Install a shared `cancel_reason` cell. Intended for the kernel
    /// run-loop to thread the channel-adapter-owned cell (carrying
    /// `CancelReason::StopPattern` after `cancel_conv` fires) into
    /// `RunCtx`, so hooks and on-stop observers see the same write-once
    /// attribution.
    #[must_use]
    pub fn with_cancel_reason(mut self, cell: Arc<OnceLock<CancelReason>>) -> Self {
        self.cancel_reason = cell;
        self
    }

    /// Return the resolved session id, if a session-persisting hook has
    /// populated it during the current run.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.get().map(String::as_str)
    }

    /// Populate the session id cell. Returns `Err` if it was already set,
    /// matching `OnceLock::set` semantics. Intended for use by
    /// session-persisting hooks during `on_session_start`.
    ///
    /// # Errors
    ///
    /// Returns the rejected value when the cell is already populated.
    pub fn set_session_id(&self, session_id: impl Into<String>) -> Result<(), String> {
        self.session_id.set(session_id.into())
    }

    /// Return the session-scoped model override, if a session-persisting
    /// hook has published one during the current run.
    #[must_use]
    pub fn session_model_override(&self) -> Option<&ModelId> {
        self.session_model_override.get()
    }

    /// Populate the session-scoped model override. Returns `Err` if it
    /// was already set, matching `OnceLock::set` semantics. Intended for
    /// use by session-persisting hooks during `on_session_start`.
    ///
    /// # Errors
    ///
    /// Returns the rejected value when the cell is already populated.
    pub fn set_session_model_override(&self, model: ModelId) -> Result<(), ModelId> {
        self.session_model_override.set(model)
    }

    /// Return the session-scoped reasoning-effort override, if a
    /// session-persisting hook has published one during the current run.
    #[must_use]
    pub fn session_reasoning_effort_override(&self) -> Option<ReasoningEffort> {
        self.session_reasoning_effort_override.get().copied()
    }

    /// Populate the session-scoped reasoning-effort override. Returns
    /// `Err` if it was already set, matching `OnceLock::set` semantics.
    /// Intended for use by session-persisting hooks during
    /// `on_session_start`.
    ///
    /// # Errors
    ///
    /// Returns the rejected value when the cell is already populated.
    pub fn set_session_reasoning_effort_override(
        &self,
        effort: ReasoningEffort,
    ) -> Result<(), ReasoningEffort> {
        self.session_reasoning_effort_override.set(effort)
    }

    /// Return the cancellation attribution, if any observer wrote one
    /// before the run reached `on_stop`.
    #[must_use]
    pub fn cancel_reason(&self) -> Option<CancelReason> {
        self.cancel_reason.get().copied()
    }

    /// Populate the cancellation-reason cell. Returns `Err(reason)` if
    /// the cell was already set, matching `OnceLock::set` semantics.
    /// Intended for channel adapters (`StopPattern`) and hooks (Hook).
    ///
    /// # Errors
    ///
    /// Returns the rejected `CancelReason` when the cell is already
    /// populated.
    pub fn set_cancel_reason(&self, reason: CancelReason) -> Result<(), CancelReason> {
        self.cancel_reason.set(reason)
    }
}

/// Hook trait. Decision-returning methods default to `Continue`; terminal
/// callbacks default to no-op.
///
/// Implementors override only the events they care about. Hooks must be
/// `Send + Sync` and stored as `Arc<dyn Hook>` so the kernel can clone
/// references cheaply.
///
/// Trust boundary: hooks can read conversation state, subject identity,
/// tool results, provider requests, and provider responses. Hooks can
/// also replace request or response state. The `HookChain` runs in
/// registration order, so that order is the trust order for mutation.
/// Subprocess hooks inherit the trust level of the script they run. See
/// the `crabgent-hook-subprocess` crate
/// when wiring hooks from outside the current process.
#[async_trait]
pub trait Hook: Send + Sync {
    async fn on_session_start(&self, _ctx: &RunCtx) -> Decision<()> {
        Decision::Continue
    }
    async fn on_user_prompt_submit(
        &self,
        _msgs: &[Message],
        _ctx: &RunCtx,
    ) -> Decision<Vec<Message>> {
        Decision::Continue
    }
    async fn before_llm(&self, _req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        Decision::Continue
    }
    async fn after_llm(
        &self,
        _req: &LlmRequest,
        _resp: &LlmResponse,
        _ctx: &RunCtx,
    ) -> Decision<LlmResponse> {
        Decision::Continue
    }
    async fn before_tool(&self, _call: &ToolCall, _ctx: &RunCtx) -> Decision<ToolCall> {
        Decision::Continue
    }
    async fn after_tool(
        &self,
        _call: &ToolCall,
        _result: &ToolResult,
        _ctx: &RunCtx,
    ) -> Decision<ToolResult> {
        Decision::Continue
    }
    /// Called after the run-loop appends to the canonical message log.
    /// `Replace` rewrites the stored log before future provider requests.
    async fn on_message(&self, _msgs: &[Message], _ctx: &RunCtx) -> Decision<Vec<Message>> {
        Decision::Continue
    }
    async fn on_event(&self, _ev: &Event, _ctx: &RunCtx) -> Decision<Event> {
        Decision::Continue
    }
    async fn pre_compact(&self, _msgs: &[Message], _ctx: &RunCtx) -> Decision<Vec<Message>> {
        Decision::Continue
    }
    async fn on_stop(&self, _ctx: &RunCtx, _outcome: &Outcome) {}
    async fn on_notification(&self, _note: &Notification, _ctx: &RunCtx) -> Decision<()> {
        Decision::Continue
    }
    async fn on_error(&self, _ctx: &RunCtx, _err: &KernelError) {}
    /// Fires once per [`crate::Kernel::shutdown`] invocation, after the
    /// kernel-wide shutdown token has been cancelled and before active
    /// runs are drained. The token is ALREADY cancelled when this hook
    /// runs; do not await `token.cancelled()` expecting a future signal.
    /// Keep work bounded: shutdown waits for these hooks before the
    /// running-task drain grace window starts.
    async fn on_kernel_shutdown(&self, _shutdown_token: &CancellationToken) {}
    /// Fires once per spawned-task `JoinError` observed while
    /// [`crate::Kernel::shutdown`] drains the running `JoinSet`. The
    /// default no-op keeps `crabgent-core` free of direct observability;
    /// `crabgent-hook-log` bridges the event to its warning log path.
    /// Keep custom impls bounded: shutdown awaits each callback before
    /// continuing the drain.
    async fn on_kernel_shutdown_task_panic(&self, _err: &JoinError) {}
}

#[cfg(test)]
mod tests;
