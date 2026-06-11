//! Free helpers used by `KernelChannelInbox` to run dispatched events.
//!
//! Split out of `inbox/mod.rs` so the module stays under the workspace
//! LOC cap while accommodating both message and reaction dispatch.

use std::sync::Arc;

use crabgent_core::Kernel;
use crabgent_core::error::KernelError;
use crabgent_core::hook::Event;
use crabgent_core::message::ContentBlock;
use crabgent_core::owner::Owner;
use crabgent_core::run::RunRequest;
use crabgent_core::sanitize::{sanitize_for_attribute, xml_escape_body};
use crabgent_core::subject::Subject;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::OwnedSemaphorePermit;

use crate::envelope::{InboundEvent, InboundReaction};
use crate::error::ChannelError;
use crate::inbox_lifecycle::{ConvKey, InboxLifecycle};
use crate::subject::attr_keys;

use super::hint::{sanitize_for_prompt, strip_denylisted};
use super::live_turn::{LiveTurnDelivery, LiveTurnState};

/// Serialize an inbound event to the JSON shape used by `InjectionRegistry`.
///
/// Produces `{"role":"user","content":[{"type":"text","text":"..."},
/// ...attachments]}` matching the `RunRequest.messages[0]` shape used by
/// `KernelChannelInbox::build_request`.
pub(super) fn event_to_inject_value(
    event: &InboundEvent,
    subject: &Subject,
) -> Result<Value, ChannelError> {
    let mut content: Vec<Value> = vec![json!({
        "type": "text",
        "text": inbound_text_body(event.body.as_str(), subject),
    })];
    for attachment in &event.attachments {
        content.push(serde_json::to_value(inbound_content_block(
            attachment, subject,
        ))?);
    }
    Ok(json!({"role": "user", "content": content}))
}

pub(super) fn inbound_content_block(block: &ContentBlock, subject: &Subject) -> ContentBlock {
    match block {
        ContentBlock::Text { text } => ContentBlock::Text {
            text: wrap_inbound_text_body(text, subject, false),
        },
        other => other.clone(),
    }
}

pub(super) fn inbound_text_body(body: &str, subject: &Subject) -> String {
    wrap_inbound_text_body(body, subject, true)
}

fn wrap_inbound_text_body(body: &str, subject: &Subject, trust_pre_wrapped: bool) -> String {
    if trust_pre_wrapped && body.starts_with("<inbound ") && body.ends_with("</inbound>") {
        return body.to_owned();
    }
    let source = subject
        .attr(attr_keys::CHANNEL_KIND)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("unknown");
    let mut attrs = format!("source=\"{}\"", sanitize_for_attribute(source));
    // source + channel are internal/fixed (channel_kind, adapter slug): the
    // quote/angle escape is enough. name/workspace/sender are adapter-supplied
    // user-controllable strings, so they additionally run through the
    // prompt-injection denylist strip before the escape, matching what
    // build_conversation_hint does for the SAME values (no producer divergence).
    push_optional_attr(&mut attrs, "channel", subject.attr(attr_keys::CHANNEL));
    push_user_attr(&mut attrs, "name", subject.attr(attr_keys::CHANNEL_DISPLAY));
    push_user_attr(
        &mut attrs,
        "workspace",
        subject.attr(attr_keys::WORKSPACE_DISPLAY),
    );
    push_user_attr(
        &mut attrs,
        "sender",
        subject.attr(attr_keys::SENDER_DISPLAY),
    );
    format!("<inbound {attrs}>{}</inbound>", xml_escape_body(body))
}

/// Append ` key="escaped"` to `attrs` when `value` is present and not
/// blank. The value is escaped via `sanitize_for_attribute` (quote/angle/amp)
/// so it cannot break out of the tag. Used for internal/fixed attrs
/// (`channel` = adapter slug) where the source string is not user-controllable.
fn push_optional_attr(attrs: &mut String, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|v| !v.trim().is_empty()) {
        push_attr(attrs, key, &sanitize_for_attribute(value));
    }
}

/// Append ` key="escaped"` for a user-controllable display value
/// (`name`/`workspace`/`sender`). The denylist strip runs FIRST (removing Cc
/// controls, Cf format chars like bidi-override / zero-width, and Zl/Zp
/// separators), then the attribute escape neutralizes quotes/angles/amp. This
/// matches `build_conversation_hint`, which already strips these chars from the
/// same adapter-supplied strings.
fn push_user_attr(attrs: &mut String, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|v| !v.trim().is_empty()) {
        push_attr(
            attrs,
            key,
            &sanitize_for_attribute(&strip_denylisted(value)),
        );
    }
}

fn push_attr(attrs: &mut String, key: &str, escaped: &str) {
    attrs.push(' ');
    attrs.push_str(key);
    attrs.push_str("=\"");
    attrs.push_str(escaped);
    attrs.push('"');
}

/// Build the `(channel, conv)` claim key for an inbound event.
pub(super) fn conv_key_for(event: &InboundEvent) -> ConvKey {
    ConvKey(event.channel.clone(), event.conv.as_str().to_owned())
}

/// Synthesise an `InboundEvent` from a reaction so the kernel can dispatch
/// it through the regular run pipeline.
///
/// The body string is sanitised so a compromised channel cannot inject
/// prompt-control characters via the channel-opaque emoji or message id.
/// Hooks distinguish reaction events from regular messages by inspecting
/// the `inbound_reaction_*` `Subject` attrs that `KernelChannelInbox`
/// stamps after this helper returns.
pub(super) fn synth_event_from_reaction(r: &InboundReaction) -> InboundEvent {
    let safe_emoji = sanitize_for_prompt(r.emoji.as_str());
    let safe_target = sanitize_for_prompt(r.parent.id.as_str());
    let verb = if r.added { "reacted" } else { "unreacted" };
    let body = format!("[user {verb} with {safe_emoji} to message {safe_target}]");
    InboundEvent {
        channel: r.channel.clone(),
        conv: r.conv.clone(),
        kind: None,
        from: r.from.clone(),
        message: r.parent.clone(),
        body,
        attachments: Vec::new(),
        timestamp: r.timestamp,
    }
}

/// Parameters for live progress and final delivery on foreground turns.
pub(super) struct LiveRunParams {
    pub live_turn: Option<LiveTurnDelivery>,
    pub subject: Subject,
    pub conv: Owner,
    pub channel: String,
}

pub(super) async fn run_kernel_with_release(
    kernel: Arc<Kernel>,
    req: RunRequest,
    cancel: tokio_util::sync::CancellationToken,
    permit: OwnedSemaphorePermit,
    lifecycle: Arc<InboxLifecycle>,
    conv_key: ConvKey,
    live_params: LiveRunParams,
) -> Result<(), KernelError> {
    let mut live_state = live_params.live_turn.map(|delivery| {
        LiveTurnState::new(
            delivery,
            live_params.subject.clone(),
            live_params.conv.clone(),
            live_params.channel.clone(),
        )
    });
    let result = run_kernel(kernel, &req, &cancel, permit, live_state.as_mut()).await;
    lifecycle.release_conv(&conv_key, &req.run_id).await;

    if let (Err(err), Some(state)) = (&result, live_state.as_mut()) {
        let cancel_reason = req
            .cancel_reason
            .as_ref()
            .and_then(|reason| reason.get().copied());
        state
            .finish_error(err, cancel_reason, lifecycle.is_shutdown())
            .await;
    }

    result
}

async fn run_kernel(
    kernel: Arc<Kernel>,
    req: &RunRequest,
    cancel: &tokio_util::sync::CancellationToken,
    permit: OwnedSemaphorePermit,
    live_state: Option<&mut LiveTurnState>,
) -> Result<(), KernelError> {
    let _permit = permit;
    match live_state {
        Some(state) => run_kernel_live(kernel, req, cancel, state).await,
        None => match kernel.run(req.clone(), Some(cancel)).await {
            Ok(_) => Ok(()),
            Err(_) if cancel.is_cancelled() => Err(KernelError::Cancelled),
            Err(err) => Err(err),
        },
    }
}

async fn run_kernel_live(
    kernel: Arc<Kernel>,
    req: &RunRequest,
    cancel: &tokio_util::sync::CancellationToken,
    live_state: &mut LiveTurnState,
) -> Result<(), KernelError> {
    let stream = kernel.run_streaming(req.clone(), Some(cancel));
    tokio::pin!(stream);
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => {
                live_state.observe(&event).await;
                if let Event::Final(text) = event {
                    live_state.finish_success(&text).await;
                    return Ok(());
                }
            }
            Err(_) if cancel.is_cancelled() => return Err(KernelError::Cancelled),
            Err(err) => return Err(err),
        }
    }
    Err(KernelError::Internal(
        "run stream ended without final event".into(),
    ))
}
