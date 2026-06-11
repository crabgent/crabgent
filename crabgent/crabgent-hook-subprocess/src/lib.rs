//! # crabgent-hook-subprocess
//!
//! `Hook` adapter that delegates kernel callbacks to an external script.
//! For each forwarded event, the adapter spawns the configured command,
//! writes one JSON envelope on stdin, and reads one JSON `Decision` from
//! stdout. The script can be Python, shell, Node, anything that reads
//! stdin and writes stdout.
//!
//! ## Wire format
//!
//! Stdin (one line):
//! ```json
//! {
//!   "event": "before_llm",
//!   "ctx": { "run_id": "01HZX...", "subject_id": "user-1" },
//!   "payload": { ...event-specific JSON... }
//! }
//! ```
//!
//! Stdout (one line):
//! ```json
//! { "decision": "continue" }
//! { "decision": "replace", "value": { ... } }
//! { "decision": "deny",    "reason": "string" }
//! ```
//!
//! ## Failure semantics
//!
//! Spawn errors, timeouts, malformed JSON, and non-zero exits map to
//! either `Decision::Deny` (default `FailureMode::Strict`) or
//! `Decision::Continue` (`FailureMode::Lenient`). A `Replace` whose
//! `value` cannot be deserialized into the target type is always denied,
//! per the kernel's fail-closed policy on malformed hook output. Terminal
//! callbacks still dispatch to the script, but a `Deny` is logged because
//! the run has already reached its terminal state. Terminal `Replace`
//! decisions are ignored because there is no terminal state to substitute.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use crabgent_core::{
    Decision, Event, Hook, LlmRequest, LlmResponse, Message, Notification, Outcome, RunCtx,
    ToolCall, ToolResult,
};
use crabgent_log::warn;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

mod proto;
mod runner;

pub use proto::FailureMode;
use proto::{HookCtx, HookInput, HookOutput};
use runner::RunnerError;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Subprocess-backed `Hook`. Construct via [`SubprocessHook::builder`].
pub struct SubprocessHook {
    inner: Arc<Inner>,
}

struct Inner {
    cmd: Vec<String>,
    timeout: Duration,
    failure_mode: FailureMode,
    events: EventFilter,
}

enum EventFilter {
    All,
    Only(HashSet<String>),
}

/// Builder for [`SubprocessHook`].
pub struct SubprocessHookBuilder {
    cmd: Vec<String>,
    timeout: Duration,
    failure_mode: FailureMode,
    events: EventFilter,
}

impl SubprocessHook {
    /// Start building a `SubprocessHook` with the given command. The
    /// command is split into program + args; e.g. `["python3",
    /// "/path/to/hook.py"]`.
    pub fn builder<I, S>(command: I) -> SubprocessHookBuilder
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        SubprocessHookBuilder {
            cmd: command.into_iter().map(Into::into).collect(),
            timeout: DEFAULT_TIMEOUT,
            failure_mode: FailureMode::default(),
            events: EventFilter::All,
        }
    }

    fn forwards(&self, event: &str) -> bool {
        match &self.inner.events {
            EventFilter::All => true,
            EventFilter::Only(set) => set.contains(event),
        }
    }

    async fn dispatch_raw(
        &self,
        event: &'static str,
        payload: Value,
        ctx: &RunCtx,
    ) -> Result<HookOutput, RunnerError> {
        let run_id = ctx.run_id.to_string();
        let input = HookInput {
            event,
            ctx: HookCtx {
                run_id: &run_id,
                subject_id: ctx.subject.id(),
            },
            payload,
        };
        let body = serde_json::to_value(&input).map_err(RunnerError::BadJson)?;
        runner::run(&self.inner.cmd, &body, self.inner.timeout).await
    }

    async fn dispatch_typed<T: DeserializeOwned>(
        &self,
        event: &'static str,
        payload: Value,
        ctx: &RunCtx,
    ) -> Decision<T> {
        if !self.forwards(event) {
            return Decision::Continue;
        }
        match self.dispatch_raw(event, payload, ctx).await {
            Ok(HookOutput::Continue) => Decision::Continue,
            Ok(HookOutput::Replace { value }) => decode_replace(value),
            Ok(HookOutput::Deny { reason }) => Decision::Deny(reason),
            Err(e) => failure_decision(self.inner.failure_mode, &e, event),
        }
    }

    async fn dispatch_unit(
        &self,
        event: &'static str,
        payload: Value,
        ctx: &RunCtx,
    ) -> Decision<()> {
        if !self.forwards(event) {
            return Decision::Continue;
        }
        // Unit callbacks have no substitutable payload, so Replace is
        // collapsed to Continue.
        match self.dispatch_raw(event, payload, ctx).await {
            Ok(HookOutput::Continue | HookOutput::Replace { .. }) => Decision::Continue,
            Ok(HookOutput::Deny { reason }) => Decision::Deny(reason),
            Err(e) => failure_decision(self.inner.failure_mode, &e, event),
        }
    }

    async fn dispatch_terminal(&self, event: &'static str, payload: Value, ctx: &RunCtx) {
        if let Decision::Deny(reason) = self.dispatch_unit(event, payload, ctx).await {
            warn!(
                run_id = %ctx.run_id,
                event,
                reason,
                "terminal subprocess hook denied after run termination"
            );
        }
    }
}

fn decode_replace<T: DeserializeOwned>(value: Value) -> Decision<T> {
    match serde_json::from_value(value) {
        Ok(v) => Decision::Replace(v),
        Err(e) => {
            warn!(error = %e, "subprocess returned malformed Replace, denying");
            Decision::Deny(format!("malformed replace: {e}"))
        }
    }
}

fn failure_decision<T>(mode: FailureMode, err: &RunnerError, event: &'static str) -> Decision<T> {
    match mode {
        FailureMode::Strict => strict_failure_decision(err, event),
        FailureMode::Lenient => lenient_failure_decision(err, event),
    }
}

fn strict_failure_decision<T>(err: &RunnerError, event: &'static str) -> Decision<T> {
    warn!(error = %err, event, "subprocess hook failed, denying");
    Decision::Deny(format!("subprocess hook error: {err}"))
}

fn lenient_failure_decision<T>(err: &RunnerError, event: &'static str) -> Decision<T> {
    warn!(error = %err, event, "subprocess hook failed, continuing");
    Decision::Continue
}

/// Best-effort serialization for terminal subprocess events.
///
/// Returns `Value::Null` if serialization fails.
fn to_value<T: Serialize>(t: &T) -> Value {
    serde_json::to_value(t).unwrap_or(Value::Null)
}

impl SubprocessHookBuilder {
    pub const fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    pub const fn failure_mode(mut self, mode: FailureMode) -> Self {
        self.failure_mode = mode;
        self
    }

    /// Forward only the named events. Pass kernel callback names as
    /// strings (`"before_llm"`, `"on_event"`, ...). Other events
    /// short-circuit to `Decision::Continue`.
    pub fn only_events<I, S>(mut self, events: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.events = EventFilter::Only(events.into_iter().map(Into::into).collect());
        self
    }

    pub fn build(self) -> SubprocessHook {
        SubprocessHook {
            inner: Arc::new(Inner {
                cmd: self.cmd,
                timeout: self.timeout,
                failure_mode: self.failure_mode,
                events: self.events,
            }),
        }
    }
}

#[async_trait]
impl Hook for SubprocessHook {
    async fn on_session_start(&self, ctx: &RunCtx) -> Decision<()> {
        self.dispatch_unit("on_session_start", Value::Null, ctx)
            .await
    }

    async fn on_user_prompt_submit(
        &self,
        msgs: &[Message],
        ctx: &RunCtx,
    ) -> Decision<Vec<Message>> {
        self.dispatch_typed("on_user_prompt_submit", to_value(&msgs), ctx)
            .await
    }

    async fn before_llm(&self, req: &LlmRequest, ctx: &RunCtx) -> Decision<LlmRequest> {
        self.dispatch_typed("before_llm", to_value(req), ctx).await
    }

    async fn after_llm(
        &self,
        req: &LlmRequest,
        resp: &LlmResponse,
        ctx: &RunCtx,
    ) -> Decision<LlmResponse> {
        let payload = serde_json::json!({"request": req, "response": resp});
        self.dispatch_typed("after_llm", payload, ctx).await
    }

    async fn before_tool(&self, call: &ToolCall, ctx: &RunCtx) -> Decision<ToolCall> {
        self.dispatch_typed("before_tool", to_value(call), ctx)
            .await
    }

    async fn after_tool(
        &self,
        call: &ToolCall,
        result: &ToolResult,
        ctx: &RunCtx,
    ) -> Decision<ToolResult> {
        let payload = serde_json::json!({"call": call, "result": result});
        self.dispatch_typed("after_tool", payload, ctx).await
    }

    async fn on_message(&self, msgs: &[Message], ctx: &RunCtx) -> Decision<Vec<Message>> {
        self.dispatch_typed("on_message", to_value(&msgs), ctx)
            .await
    }

    async fn on_event(&self, ev: &Event, ctx: &RunCtx) -> Decision<Event> {
        self.dispatch_typed("on_event", to_value(ev), ctx).await
    }

    async fn pre_compact(&self, msgs: &[Message], ctx: &RunCtx) -> Decision<Vec<Message>> {
        self.dispatch_typed("pre_compact", to_value(&msgs), ctx)
            .await
    }

    /// Serializes the kernel [`Outcome`] onto the script wire verbatim.
    /// Wire kinds: `completed`, `max_turns_exceeded`, `cancelled`,
    /// `paused`, `errored`. Scripts matching the kind with a strict enum
    /// must accept `paused` (added with task/goal pause support; pre-1.0
    /// clean break, no shim).
    async fn on_stop(&self, ctx: &RunCtx, outcome: &Outcome) {
        self.dispatch_terminal("on_stop", to_value(outcome), ctx)
            .await;
    }

    async fn on_notification(&self, note: &Notification, ctx: &RunCtx) -> Decision<()> {
        self.dispatch_unit("on_notification", to_value(note), ctx)
            .await
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
