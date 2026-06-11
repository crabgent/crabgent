//! # crabgent-hook-inject
//!
//! Runtime message injection as a hook. External code (a UI button, a
//! companion agent, an operator) submits messages into an
//! [`InjectionRegistry`] keyed by [`RunId`]. The [`InjectHook`] picks
//! pending messages up at the next `before_llm` callback and splices
//! them into the outgoing [`LlmRequest`]. When no user anchor exists,
//! the hook keeps the old append fallback for synthetic histories.
//!
//! ```ignore
//! use crabgent_hook_inject::{InjectHook, InjectionRegistry};
//! use crabgent_core::RunId;
//!
//! let registry = InjectionRegistry::new();
//! let _hook = InjectHook::new(registry.clone());
//! let run_id = RunId::new();
//! registry.submit_user_text(&run_id, "ignore previous; do X").await;
//! ```
//!
//! Pending messages survive across LLM calls until drained. The hook
//! also clears the registry entry on `on_stop` so finished runs do not
//! leak memory.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::message::tail::unresolved_tail_boundary;
use crabgent_core::{Decision, Hook, LlmRequest, Message, Outcome, RunCtx, RunId, ToolResult};
use crabgent_log::info;
use serde_json::{Value, json};
use tokio::sync::Mutex;

/// Concurrency-safe queue of pending injected messages, keyed by
/// [`RunId`]. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct InjectionRegistry {
    inner: Arc<Mutex<HashMap<RunId, VecDeque<Value>>>>,
}

impl InjectionRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn submit(&self, run_id: &RunId, message: Value) {
        let mut guard = self.inner.lock().await;
        guard.entry(run_id.clone()).or_default().push_back(message);
    }

    pub async fn submit_user_text(&self, run_id: &RunId, text: impl Into<String>) {
        let v = json!({
            "role": "user",
            "content": [{"type": "text", "text": text.into()}],
        });
        self.submit(run_id, v).await;
    }

    pub async fn submit_system_text(&self, run_id: &RunId, text: impl Into<String>) {
        let v = json!({"role": "system", "content": text.into()});
        self.submit(run_id, v).await;
    }

    /// Convenience: enqueue an assistant message with plain text and
    /// no tool calls.
    pub async fn submit_assistant_text(&self, run_id: &RunId, text: impl Into<String>) {
        let message = Message::Assistant {
            text: text.into(),
            tool_calls: Vec::new(),
        };
        self.submit(
            run_id,
            // invariant: Message::Assistant is a concrete enum variant of
            // String + Vec<ToolCall>, no floats, no non-string map keys, so
            // serde_json serialization is infallible.
            serde_json::to_value(message).expect("assistant message serialisable"),
        )
        .await;
    }

    /// Convenience: enqueue a tool result message.
    pub async fn submit_tool_result(
        &self,
        run_id: &RunId,
        call_id: impl Into<String>,
        output: Value,
        is_error: bool,
    ) {
        let result = ToolResult {
            call_id: call_id.into(),
            output,
            is_error,
            run_messages: Vec::new(),
        };
        self.submit(
            run_id,
            // invariant: Message::ToolResult holds String + serde_json::Value
            // + bool. The Value came from a successfully deserialized tool
            // output, so re-serializing the wrapping enum is infallible.
            serde_json::to_value(Message::ToolResult {
                call_id: result.call_id,
                output: result.output,
                is_error: result.is_error,
            })
            .expect("tool result serialisable"),
        )
        .await;
    }

    pub async fn pending(&self, run_id: &RunId) -> usize {
        let guard = self.inner.lock().await;
        guard.get(run_id).map_or(0, VecDeque::len)
    }

    /// Drop all pending messages for a run.
    pub async fn clear(&self, run_id: &RunId) {
        let mut guard = self.inner.lock().await;
        guard.remove(run_id);
    }

    async fn drain(&self, run_id: &RunId) -> Vec<Value> {
        let mut guard = self.inner.lock().await;
        let Some(q) = guard.get_mut(run_id) else {
            return Vec::new();
        };
        std::mem::take(q).into_iter().collect()
    }
}

impl Default for InjectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ASCII marker for fresh mid-turn user input. U+2014 is forbidden in source.
pub(crate) const MID_TURN_MARKER: &str =
    "[NEW USER INPUT mid-turn: address before ending this turn]";

// Prepend MID_TURN_MARKER to the first text-bearing position of a user value.
fn prepend_mid_turn_marker(value: &mut Value) {
    if value.get("role").and_then(Value::as_str) != Some("user") {
        return;
    }
    let Some(content) = value.get_mut("content") else {
        return;
    };
    match content {
        Value::Array(blocks) => {
            for block in &mut *blocks {
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                let Some(text_field) = block.get_mut("text") else {
                    continue;
                };
                let Some(text) = text_field.as_str() else {
                    continue;
                };
                if text.starts_with(MID_TURN_MARKER) {
                    return;
                }
                *text_field = Value::String(format!("{MID_TURN_MARKER} {text}"));
                return;
            }
        }
        Value::String(text) => {
            if text.starts_with(MID_TURN_MARKER) {
                return;
            }
            *text = format!("{MID_TURN_MARKER} {text}");
        }
        _ => {}
    }
}

fn has_user_anchor(messages: &[Value]) -> bool {
    messages
        .iter()
        .any(|m| m.get("role").and_then(Value::as_str) == Some("user"))
}

/// `Hook` that drains pending injections from an [`InjectionRegistry`]
/// at every `before_llm` callback and clears the registry on `on_stop`.
pub struct InjectHook {
    registry: InjectionRegistry,
}

impl InjectHook {
    pub const fn new(registry: InjectionRegistry) -> Self {
        Self { registry }
    }

    pub const fn registry(&self) -> &InjectionRegistry {
        &self.registry
    }
}

#[async_trait]
impl Hook for InjectHook {
    async fn before_llm(&self, req: &LlmRequest, ctx: &RunCtx) -> Decision<LlmRequest> {
        let mut pending = self.registry.drain(&ctx.run_id).await;
        if pending.is_empty() {
            return Decision::Continue;
        }
        info!(
            run_id = %ctx.run_id,
            count = pending.len(),
            "injecting pending messages into LlmRequest",
        );
        for item in &mut pending {
            prepend_mid_turn_marker(item);
        }
        let mut new_req = req.clone();
        if has_user_anchor(&new_req.messages) {
            let idx = unresolved_tail_boundary(&new_req.messages);
            new_req.messages.splice(idx..idx, pending);
        } else {
            new_req.messages.extend(pending);
        }
        Decision::Replace(new_req)
    }

    async fn on_stop(&self, ctx: &RunCtx, _outcome: &Outcome) {
        self.registry.clear(&ctx.run_id).await;
    }
}

#[cfg(test)]
mod tests;
