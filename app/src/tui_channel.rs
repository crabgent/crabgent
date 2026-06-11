//! In-process TUI channel adapter.
//!
//! This is not the tmux bridge. It routes `notify_user(channel="tui",
//! participant_id="<agent>")` into active `/tui/<agent>` WebSocket clients so
//! out-of-band agent messages appear in the TUI itself.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_channel::participant::{Participant, ParticipantId, ParticipantRole};
use crabgent_channel::{
    Channel, ChannelError, ChannelKind, MessageRef, OutboundMessage, ReadMessage,
};
use crabgent_core::Owner;
use crabgent_core::Subject;
use tokio::sync::{RwLock, broadcast};

pub const CHANNEL_NAME: &str = "tui";

const CHANNEL_CAPACITY: usize = 128;
const BACKLOG_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiDelivery {
    pub from: String,
    pub body: String,
}

#[derive(Debug, Default, Clone)]
pub struct TuiHub {
    state: Arc<RwLock<TuiHubState>>,
}

#[derive(Debug, Default)]
struct TuiHubState {
    topics: HashMap<String, broadcast::Sender<TuiDelivery>>,
    backlog: HashMap<String, VecDeque<TuiDelivery>>,
}

impl TuiHub {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn subscribe_with_backlog(
        &self,
        participant: &str,
    ) -> (broadcast::Receiver<TuiDelivery>, Vec<TuiDelivery>) {
        let mut state = self.state.write().await;
        let rx = state
            .topics
            .entry(participant.to_owned())
            .or_insert_with(|| {
                let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
                tx
            })
            .subscribe();
        let backlog = state
            .backlog
            .remove(participant)
            .map(|items| items.into_iter().collect())
            .unwrap_or_default();
        drop(state);
        (rx, backlog)
    }

    pub async fn publish(
        &self,
        participant: &str,
        delivery: TuiDelivery,
    ) -> Result<(), ChannelError> {
        self.deliver(participant, delivery).await
    }

    async fn deliver(&self, participant: &str, delivery: TuiDelivery) -> Result<(), ChannelError> {
        let tx = {
            let mut state = self.state.write().await;
            let tx = state.topics.get(participant).cloned();
            let has_receiver = tx
                .as_ref()
                .is_some_and(|sender| sender.receiver_count() > 0);
            if !has_receiver {
                let backlog = state.backlog.entry(participant.to_owned()).or_default();
                backlog.push_back(delivery.clone());
                while backlog.len() > BACKLOG_CAPACITY {
                    backlog.pop_front();
                }
            }
            drop(state);
            tx
        };
        let Some(tx) = tx else {
            return Ok(());
        };
        let _ = tx.send(delivery);
        Ok(())
    }
}

pub struct TuiChannel {
    hub: TuiHub,
}

impl TuiChannel {
    #[must_use]
    pub const fn new(hub: TuiHub) -> Self {
        Self { hub }
    }
}

pub fn normalize_tui_body(body: &str) -> String {
    if !looks_like_html(body) {
        return body.to_owned();
    }
    compact_rendered_html(&render_htmlish_body(body))
}

fn looks_like_html(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    [
        "<p", "</p", "<ul", "</ul", "<ol", "</ol", "<li", "</li", "<br", "<code", "</code", "<pre",
        "</pre", "<b", "</b", "<strong", "</strong", "<i", "</i", "<em", "</em",
    ]
    .iter()
    .any(|tag| lower.contains(tag))
}

fn render_htmlish_body(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find('<') {
        push_decoded_entities(&mut out, &rest[..start]);
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('>') else {
            out.push('<');
            rest = after_start;
            continue;
        };
        push_html_tag(&mut out, &after_start[..end]);
        rest = &after_start[end + 1..];
    }
    push_decoded_entities(&mut out, rest);
    out
}

fn push_html_tag(out: &mut String, raw: &str) {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('!') {
        return;
    }
    let closing = raw.starts_with('/');
    let tag = raw
        .trim_start_matches('/')
        .trim_end_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match (closing, tag.as_str()) {
        (_, "p" | "ul" | "ol") => push_blank_line(out),
        (false, "li") => {
            push_newline(out);
            out.push_str("- ");
        }
        (true, "li") | (false, "br") => push_newline(out),
        (_, "code") => out.push('`'),
        (false, "pre") => {
            push_blank_line(out);
            out.push_str("```text\n");
        }
        (true, "pre") => {
            push_newline(out);
            out.push_str("```\n");
        }
        (_, "b" | "strong") => out.push_str("**"),
        (_, "i" | "em") => out.push('*'),
        _ => {}
    }
}

fn push_decoded_entities(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find(';') else {
            out.push('&');
            rest = after_start;
            continue;
        };
        let entity = &after_start[..end];
        if let Some(decoded) = decode_html_entity(entity) {
            out.push(decoded);
        } else {
            out.push('&');
            out.push_str(entity);
            out.push(';');
        }
        rest = &after_start[end + 1..];
    }
    out.push_str(rest);
}

fn decode_html_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" | "#39" => Some('\''),
        entity if entity.starts_with("#x") || entity.starts_with("#X") => {
            u32::from_str_radix(&entity[2..], 16)
                .ok()
                .and_then(char::from_u32)
        }
        entity if entity.starts_with('#') => entity[1..].parse().ok().and_then(char::from_u32),
        _ => None,
    }
}

fn push_newline(out: &mut String) {
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

fn push_blank_line(out: &mut String) {
    let trimmed = out.trim_end_matches(' ');
    if trimmed.is_empty() {
        out.truncate(trimmed.len());
        return;
    }
    out.truncate(trimmed.len());
    if out.ends_with("\n\n") {
        return;
    }
    if out.ends_with('\n') {
        out.push('\n');
    } else {
        out.push_str("\n\n");
    }
}

fn compact_rendered_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_lines = 0usize;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            blank_lines = blank_lines.saturating_add(1);
            if blank_lines <= 1 && !out.is_empty() {
                out.push('\n');
            }
            continue;
        }
        blank_lines = 0;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(trimmed.trim_start());
        out.push('\n');
    }
    out.trim().to_owned()
}

#[async_trait]
impl Channel for TuiChannel {
    fn name(&self) -> &'static str {
        CHANNEL_NAME
    }

    async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, ChannelError> {
        Ok(ChannelKind::Direct)
    }

    async fn participants(
        &self,
        _ctx: &Subject,
        conv: &Owner,
    ) -> Result<Vec<Participant>, ChannelError> {
        let participant = tui_participant_from_conv(conv)?;
        Ok(vec![Participant::new(
            ParticipantId::new(participant),
            ParticipantRole::Human,
        )])
    }

    async fn send(
        &self,
        ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let topic = tui_topic_from_conv(conv)?;
        self.hub
            .deliver(
                topic,
                TuiDelivery {
                    from: tui_sender(ctx),
                    body: msg.body.clone(),
                },
            )
            .await?;
        Ok(MessageRef::top_level(
            CHANNEL_NAME,
            conv.clone(),
            new_msg_id(),
        ))
    }

    async fn notify_user(
        &self,
        ctx: &Subject,
        recipient: &ParticipantId,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let participant = recipient.as_ref();
        let (conv, topic) = notify_tui_target(ctx, participant);
        self.hub
            .deliver(
                &topic,
                TuiDelivery {
                    from: tui_sender(ctx),
                    body: msg.body.clone(),
                },
            )
            .await?;
        Ok(MessageRef::top_level(CHANNEL_NAME, conv, new_msg_id()))
    }

    async fn read(
        &self,
        _ctx: &Subject,
        _conv: &Owner,
        _thread_parent: Option<&MessageRef>,
        _limit: usize,
    ) -> Result<Vec<ReadMessage>, ChannelError> {
        Err(ChannelError::Unsupported("read"))
    }
}

fn tui_participant_from_conv(conv: &Owner) -> Result<&str, ChannelError> {
    let topic = tui_topic_from_conv(conv)?;
    Ok(topic
        .split_once('/')
        .map_or(topic, |(participant, _)| participant))
}

fn tui_topic_from_conv(conv: &Owner) -> Result<&str, ChannelError> {
    tui_topic_from_str(conv.as_str())
}

fn tui_topic_from_str(conv: &str) -> Result<&str, ChannelError> {
    conv.strip_prefix("tui:")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ChannelError::InvalidOwnerFormat(conv.to_owned()))
}

fn notify_tui_target(ctx: &Subject, participant: &str) -> (Owner, String) {
    let main_conv = Owner::new(format!("{CHANNEL_NAME}:{participant}"));
    let Some(conv) = ctx.attr("conv") else {
        return (main_conv, participant.to_owned());
    };
    let Ok(topic) = tui_topic_from_str(conv) else {
        return (main_conv, participant.to_owned());
    };
    if topic.split_once('/').map_or(topic, |(agent, _)| agent) != participant {
        return (main_conv, participant.to_owned());
    }
    (Owner::new(conv), topic.to_owned())
}

fn tui_sender(ctx: &Subject) -> String {
    ctx.attr("agent")
        .map(str::to_owned)
        .or_else(|| ctx.id().strip_prefix("agent:").map(str::to_owned))
        .unwrap_or_else(|| ctx.id().to_owned())
}

fn new_msg_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_channel::OutboundMessage;

    #[tokio::test]
    async fn notify_user_delivers_to_active_tui_subscriber() {
        let hub = TuiHub::new();
        let (mut rx, backlog) = hub.subscribe_with_backlog("local").await;
        assert!(backlog.is_empty());
        let channel = TuiChannel::new(hub);
        let ctx = Subject::new("agent:nova").with_attr("agent", "nova");
        channel
            .notify_user(
                &ctx,
                &ParticipantId::new("local"),
                &OutboundMessage::new("hi"),
            )
            .await
            .expect("deliver");

        let delivery = rx.recv().await.expect("delivery");
        assert_eq!(delivery.from, "nova");
        assert_eq!(delivery.body, "hi");
    }

    #[tokio::test]
    async fn notify_user_from_named_tui_context_delivers_to_named_session() {
        let hub = TuiHub::new();
        let (mut named_rx, named_backlog) = hub.subscribe_with_backlog("local/moss").await;
        let (_main_rx, main_backlog) = hub.subscribe_with_backlog("local").await;
        assert!(named_backlog.is_empty());
        assert!(main_backlog.is_empty());
        let channel = TuiChannel::new(hub);
        let ctx = Subject::new("tui:local")
            .with_attr("agent", "local")
            .with_attr("channel", "tui")
            .with_attr("conv", "tui:local/moss");

        let sent = channel
            .notify_user(
                &ctx,
                &ParticipantId::new("local"),
                &OutboundMessage::new("fertig"),
            )
            .await
            .expect("deliver");

        assert_eq!(sent.conv.as_str(), "tui:local/moss");
        let delivery = named_rx.recv().await.expect("delivery");
        assert_eq!(delivery.from, "local");
        assert_eq!(delivery.body, "fertig");
    }

    #[tokio::test]
    async fn notify_user_from_other_context_uses_main_session() {
        let hub = TuiHub::new();
        let (mut rx, backlog) = hub.subscribe_with_backlog("local").await;
        assert!(backlog.is_empty());
        let channel = TuiChannel::new(hub);
        let ctx = Subject::new("matrix:@alice:example")
            .with_attr("agent", "local")
            .with_attr("channel", "matrix")
            .with_attr("conv", "matrix:!room");

        let sent = channel
            .notify_user(
                &ctx,
                &ParticipantId::new("local"),
                &OutboundMessage::new("hi"),
            )
            .await
            .expect("deliver");

        assert_eq!(sent.conv.as_str(), "tui:local");
        let delivery = rx.recv().await.expect("delivery");
        assert_eq!(delivery.body, "hi");
    }

    #[test]
    fn normalize_tui_body_converts_matrix_html_to_markdown() {
        let body = "<p>Done.</p><ul><li>Found: <code>meeting.org</code></li><li>Next &amp; final</li></ul>";

        let normalized = normalize_tui_body(body);

        assert_eq!(
            normalized,
            "Done.\n\n- Found: `meeting.org`\n- Next & final"
        );
    }

    #[test]
    fn normalize_tui_body_leaves_plain_markdown_untouched() {
        let body = "Done.\n\n- Found: `meeting.org`";

        assert_eq!(normalize_tui_body(body), body);
    }

    #[tokio::test]
    async fn channel_send_uses_named_tui_topic() {
        let hub = TuiHub::new();
        let (mut rx, backlog) = hub.subscribe_with_backlog("local/moss").await;
        assert!(backlog.is_empty());
        let channel = TuiChannel::new(hub);
        let ctx = Subject::new("agent:local").with_attr("agent", "local");
        let conv = Owner::new("tui:local/moss");

        channel
            .send(&ctx, &conv, &OutboundMessage::new("hi"))
            .await
            .expect("deliver");

        let delivery = rx.recv().await.expect("delivery");
        assert_eq!(delivery.body, "hi");
    }

    #[tokio::test]
    async fn notify_user_buffers_without_active_tui_subscriber() {
        let channel = TuiChannel::new(TuiHub::new());
        channel
            .notify_user(
                &Subject::new("agent:nova"),
                &ParticipantId::new("local"),
                &OutboundMessage::new("hi"),
            )
            .await
            .expect("buffered delivery");
    }

    #[tokio::test]
    async fn subscribe_with_backlog_replays_buffered_notifications() {
        let hub = TuiHub::new();
        hub.publish(
            "local",
            TuiDelivery {
                from: "nova".to_owned(),
                body: "late".to_owned(),
            },
        )
        .await
        .expect("publish");

        let (_rx, backlog) = hub.subscribe_with_backlog("local").await;

        assert_eq!(
            backlog,
            vec![TuiDelivery {
                from: "nova".to_owned(),
                body: "late".to_owned()
            }]
        );

        let (_rx, backlog) = hub.subscribe_with_backlog("local").await;
        assert!(backlog.is_empty());
    }
}
