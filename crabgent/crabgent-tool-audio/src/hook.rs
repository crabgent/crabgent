//! [`AudioHintHook`]: advertise `hear_again` to the chat model.
//!
//! Provider projection strips `source_audio` from the prompt, so the chat
//! model never sees the retained-audio handle and cannot pass it back. This
//! `before_llm` hook surfaces the handle: when the run carries a user
//! `transcript` block with retained audio, it appends one trust-fenced text
//! block to that message naming the `AudioRef` and how to call `hear_again`.
//!
//! The hint is a separate `text` content block, never a mutation of the
//! transcript block's own `text`. That keeps it disjoint from
//! [`crabgent_prosody::ProsodyHook`], which prepends a `<voice .../>` tag into
//! the transcript block's `text`: the two hooks annotate the same message
//! without colliding on a shared field. The hook is idempotent and bounded:
//! every run first strips prior hint blocks (identified by a private sentinel)
//! and then re-adds exactly one on the most-recent audio-bearing message. It
//! never touches `system_prompt`.

use async_trait::async_trait;
use crabgent_core::sanitize::xml_escape_body;
use crabgent_core::{Decision, Hook, LlmRequest, RunCtx};
use serde_json::{Value, json};

/// Marks the injected hint block so repeated `before_llm` passes can strip and
/// re-add exactly one. Private and ASCII; not user-reachable markup.
const HINT_SENTINEL: &str = "[crabgent:audio-hint]";

/// Stateless hook that advertises the `hear_again` tool when retained audio is
/// present in the run.
pub struct AudioHintHook;

impl AudioHintHook {
    /// Construct the hook.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for AudioHintHook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Hook for AudioHintHook {
    async fn before_llm(&self, req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        let mut next = req.clone();
        let stripped = strip_prior_hints(&mut next.messages);
        let added = annotate_latest_audio(&mut next.messages);
        if stripped || added {
            Decision::Replace(next)
        } else {
            Decision::Continue
        }
    }
}

/// Drop every prior hint block from all user messages. Returns whether any was
/// removed. Keeps the hint count bounded to one across repeated passes.
fn strip_prior_hints(messages: &mut [Value]) -> bool {
    let mut removed = false;
    for msg in messages.iter_mut() {
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        let before = content.len();
        content.retain(|block| !is_hint_block(block));
        removed |= content.len() != before;
    }
    removed
}

/// A `text` block carrying a previously injected hint.
fn is_hint_block(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("text")
        && block
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.starts_with(HINT_SENTINEL))
}

/// Append one hint block to the CURRENT turn's user message when it carries a
/// transcript with retained audio. Returns whether a hint was added.
///
/// Correlation + TTL (Hardening design 5b): the hint binds to the latest user message
/// only. A transcript from an earlier turn must not keep surfacing a hint on
/// every later text turn, so if the most-recent user message carries no
/// retained audio, no hint is added even when an older message still does.
fn annotate_latest_audio(messages: &mut [Value]) -> bool {
    let Some(last_user) = messages.iter().rposition(is_user_message) else {
        return false;
    };
    let audio_ref = messages
        .get(last_user)
        .and_then(|msg| msg.get("content"))
        .and_then(Value::as_array)
        .and_then(|content| latest_source_audio(content));
    let Some(audio_ref) = audio_ref else {
        return false;
    };
    if let Some(content) = messages
        .get_mut(last_user)
        .and_then(|msg| msg.get_mut("content"))
        .and_then(Value::as_array_mut)
    {
        content.push(render_hint(&audio_ref));
        return true;
    }
    false
}

/// Whether a message has the `user` role.
fn is_user_message(msg: &Value) -> bool {
    msg.get("role").and_then(Value::as_str) == Some("user")
}

/// The non-empty `source_audio` of the last transcript block in a message.
fn latest_source_audio(content: &[Value]) -> Option<String> {
    content.iter().rev().find_map(|block| {
        if block.get("type").and_then(Value::as_str) != Some("transcript") {
            return None;
        }
        block
            .get("source_audio")
            .and_then(Value::as_str)
            .filter(|handle| !handle.is_empty())
            .map(str::to_owned)
    })
}

/// Render the trust-fenced hint block. The handle is store-generated, but the
/// value is escaped defensively before it enters the prompt.
fn render_hint(audio_ref: &str) -> Value {
    let escaped = xml_escape_body(audio_ref);
    json!({
        "type": "text",
        "text": format!(
            "{HINT_SENTINEL} Audio attached (ref=\"{escaped}\"). Call \
             hear_again(audio_ref=\"{escaped}\", question=\"...\") to inspect tone, \
             pauses, emphasis, or mumbled words the text transcript may have lost."
        )
    })
}
