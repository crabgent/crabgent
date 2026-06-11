//! Message envelopes and references that flow between adapters and
//! the kernel.
//!
//! Threading is opaque: `MessageRef` carries a `thread_root: Option<String>`
//! so an adapter can mark the message as a thread-reply without
//! exposing per-channel details (Slack `thread_ts`, Telegram
//! `reply_to_message_id`, ...). `MessageRef::kind()` projects this
//! into `MessageKind::TopLevel` vs
//! `MessageKind::ThreadReply { root, broadcast }` for callers that need
//! to discriminate.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use crabgent_core::owner::Owner;

use crabgent_core::message::ContentBlock;

use crate::channel::ChannelKind;
use crate::participant::Participant;

/// Reference to a single message inside a conversation.
///
/// The `id` and `thread_root` fields are channel-opaque strings: the
/// adapter chooses the encoding (Slack uses `ts`, Telegram uses the
/// numeric message id, ...). Callers should not parse them.
///
/// `broadcast` is meaningful only for thread replies. Slack maps it to
/// `reply_broadcast`; adapters without an equivalent keep it `false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRef {
    /// Channel name (matches `Channel::name`).
    pub channel: String,
    /// Conversation that contains this message.
    pub conv: Owner,
    /// Channel-opaque message identifier.
    pub id: String,
    /// `Some(root_id)` if this message is a reply inside a thread.
    pub thread_root: Option<String>,
    /// `true` if the thread reply should also appear in the parent timeline.
    pub broadcast: bool,
}

impl MessageRef {
    /// Build a top-level message reference.
    pub fn top_level(channel: impl Into<String>, conv: Owner, id: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            conv,
            id: id.into(),
            thread_root: None,
            broadcast: false,
        }
    }

    /// Build a thread-reply message reference.
    pub fn thread_reply(
        channel: impl Into<String>,
        conv: Owner,
        id: impl Into<String>,
        root: impl Into<String>,
    ) -> Self {
        Self::thread_reply_broadcast(channel, conv, id, root, false)
    }

    /// Build a thread-reply message reference with explicit broadcast state.
    pub fn thread_reply_broadcast(
        channel: impl Into<String>,
        conv: Owner,
        id: impl Into<String>,
        root: impl Into<String>,
        broadcast: bool,
    ) -> Self {
        Self {
            channel: channel.into(),
            conv,
            id: id.into(),
            thread_root: Some(root.into()),
            broadcast,
        }
    }

    /// `true` if this message should be broadcast outside its thread.
    #[must_use]
    pub const fn broadcast(&self) -> bool {
        self.broadcast
    }

    /// Return the thread root as a borrowed string when present.
    #[must_use]
    pub fn thread_root(&self) -> Option<&str> {
        self.thread_root.as_deref()
    }

    /// Return the thread root when present, otherwise the message id.
    #[must_use]
    pub fn thread_root_or_id(&self) -> &str {
        self.thread_root().unwrap_or(self.id.as_str())
    }

    /// Project the threading state into the typed `MessageKind`.
    #[must_use]
    pub fn kind(&self) -> MessageKind {
        match self.thread_root.as_ref() {
            Some(root) => MessageKind::ThreadReply {
                root: root.clone(),
                broadcast: self.broadcast,
            },
            None => MessageKind::TopLevel,
        }
    }

    /// `true` if this message is a thread-reply.
    #[must_use]
    pub const fn is_thread_reply(&self) -> bool {
        self.thread_root.is_some()
    }
}

/// Threading classification for a `MessageRef`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageKind {
    /// A top-level conversation message.
    TopLevel,
    /// A reply inside a thread, anchored to `root`.
    ThreadReply {
        /// Channel-opaque root message id.
        root: String,
        /// Whether the reply should also appear in the parent timeline.
        broadcast: bool,
    },
}

/// Outbound message ready to be sent through a `Channel`.
#[derive(Debug, Clone, Default)]
pub struct OutboundMessage {
    /// Caller-facing body text.
    ///
    /// Callers may pass plain text or basic Markdown. Channel adapters are the
    /// single source of truth for normalizing this body into their wire format,
    /// such as Slack `mrkdwn`, Matrix `org.matrix.custom.html`, or Telegram
    /// Bot API HTML.
    pub body: String,
    /// `Some(parent)` to send as a thread-reply to `parent`.
    pub thread_parent: Option<MessageRef>,
    /// Channel-specific extras (for example `channel` on routed
    /// `notify_user`, or slack `mrkdwn=false`). Adapters interpret these
    /// freely, but callers should not use metadata to bypass adapter
    /// formatting.
    pub metadata: HashMap<String, String>,
}

impl OutboundMessage {
    /// Build a top-level outbound message.
    pub fn new(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            thread_parent: None,
            metadata: HashMap::new(),
        }
    }

    /// Anchor this message as a reply to `parent`, returning self.
    #[must_use]
    pub fn in_thread(mut self, parent: MessageRef) -> Self {
        self.thread_parent = Some(parent);
        self
    }

    /// Attach an adapter-specific metadata entry.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// An inbound event delivered by an adapter to a `ChannelInbox`.
#[derive(Debug, Clone)]
pub struct InboundEvent {
    /// Channel name (matches `Channel::name`).
    pub channel: String,
    /// Conversation the event belongs to.
    pub conv: Owner,
    /// Conversation kind when the adapter has resolved it.
    pub kind: Option<ChannelKind>,
    /// Sender of the message.
    pub from: Participant,
    /// Reference to the inbound message.
    pub message: MessageRef,
    /// Body text.
    pub body: String,
    /// Image attachments (validated and cached by the adapter).
    /// Empty by default; adapters populate this when they receive
    /// image messages from the upstream channel.
    pub attachments: Vec<ContentBlock>,
    /// When the channel observed the message.
    pub timestamp: DateTime<Utc>,
}

/// An inbound reaction event delivered by an adapter to a `ChannelInbox`.
///
/// Reactions reach the kernel through [`ChannelInbox::receive_reaction`].
/// The emoji string is channel-opaque: unicode for Matrix and Slack, the
/// Telegram `ReactionTypeEmoji.emoji` value, etc. Adapters drop reactions
/// they cannot map (custom emoji, paid reactions, anonymous reactions
/// without a sender).
#[derive(Debug, Clone)]
pub struct InboundReaction {
    /// Channel name (matches `Channel::name`).
    pub channel: String,
    /// Conversation that contains the reacted-to message.
    pub conv: Owner,
    /// Sender of the reaction.
    pub from: Participant,
    /// Reference to the message that was reacted to.
    pub parent: MessageRef,
    /// Channel-opaque emoji identifier (unicode or short-name).
    pub emoji: String,
    /// `true` when the reaction was added, `false` on removal.
    pub added: bool,
    /// When the channel observed the reaction.
    pub timestamp: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::participant::ParticipantRole;

    #[test]
    fn top_level_ref_has_no_thread_root() {
        let r = MessageRef::top_level("slack", Owner::new("slack:T1/C1"), "ts:1");
        assert!(r.thread_root.is_none());
        assert!(!r.broadcast());
        assert!(!r.is_thread_reply());
        assert_eq!(r.kind(), MessageKind::TopLevel);
    }

    #[test]
    fn thread_reply_ref_carries_root() {
        let r = MessageRef::thread_reply("slack", Owner::new("slack:T1/C1"), "ts:2", "ts:1");
        assert!(r.is_thread_reply());
        assert!(!r.broadcast());
        assert_eq!(r.thread_root(), Some("ts:1"));
        assert_eq!(
            r.kind(),
            MessageKind::ThreadReply {
                root: "ts:1".into(),
                broadcast: false
            }
        );
    }

    #[test]
    fn message_ref_thread_root_or_id() {
        let top_level = MessageRef::top_level("slack", Owner::new("slack:T1/C1"), "ts:1");
        let reply = MessageRef::thread_reply("slack", Owner::new("slack:T1/C1"), "ts:2", "ts:1");

        assert_eq!(top_level.thread_root_or_id(), "ts:1");
        assert_eq!(reply.thread_root_or_id(), "ts:1");
    }

    #[test]
    fn thread_reply_broadcast_projects_flag() {
        let r = MessageRef::thread_reply_broadcast(
            "slack",
            Owner::new("slack:T1/C1"),
            "ts:2",
            "ts:1",
            true,
        );
        assert!(r.broadcast());
        assert_eq!(
            r.kind(),
            MessageKind::ThreadReply {
                root: "ts:1".into(),
                broadcast: true
            }
        );
    }

    #[test]
    fn message_ref_equal_for_same_fields() {
        let a = MessageRef::top_level("slack", Owner::new("c"), "1");
        let b = MessageRef::top_level("slack", Owner::new("c"), "1");
        assert_eq!(a, b);
    }

    #[test]
    fn message_ref_unequal_for_different_thread_root() {
        let a = MessageRef::top_level("slack", Owner::new("c"), "1");
        let b = MessageRef::thread_reply("slack", Owner::new("c"), "1", "0");
        assert_ne!(a, b);
    }

    #[test]
    fn outbound_default_top_level_no_metadata() {
        let m = OutboundMessage::new("hi");
        assert_eq!(m.body, "hi");
        assert!(m.thread_parent.is_none());
        assert!(m.metadata.is_empty());
    }

    #[test]
    fn outbound_in_thread_attaches_parent() {
        let parent = MessageRef::top_level("slack", Owner::new("c"), "1");
        let m = OutboundMessage::new("reply").in_thread(parent.clone());
        assert_eq!(m.thread_parent.as_ref(), Some(&parent));
    }

    #[test]
    fn outbound_with_metadata_keeps_value() {
        let m = OutboundMessage::new("body")
            .with_metadata("parse_mode", "HTML")
            .with_metadata("mrkdwn", "false");
        assert_eq!(
            m.metadata.get("parse_mode").map(String::as_str),
            Some("HTML")
        );
        assert_eq!(m.metadata.get("mrkdwn").map(String::as_str), Some("false"));
    }

    #[test]
    fn outbound_default_via_default_trait() {
        let m: OutboundMessage = OutboundMessage::default();
        assert!(m.body.is_empty());
        assert!(m.thread_parent.is_none());
        assert!(m.metadata.is_empty());
    }

    #[test]
    fn inbound_event_carries_all_fields() {
        let p = Participant::new("U1", ParticipantRole::Human);
        let r = MessageRef::top_level("slack", Owner::new("slack:T1/D1"), "ts:42");
        let when = Utc::now();
        let ev = InboundEvent {
            channel: "slack".into(),
            conv: Owner::new("slack:T1/D1"),
            kind: Some(ChannelKind::Direct),
            from: p.clone(),
            message: r.clone(),
            body: "hi".into(),
            attachments: vec![],
            timestamp: when,
        };
        assert_eq!(ev.channel, "slack");
        assert_eq!(ev.kind, Some(ChannelKind::Direct));
        assert_eq!(ev.from, p);
        assert_eq!(ev.message, r);
        assert_eq!(ev.body, "hi");
        assert_eq!(ev.timestamp, when);
    }

    #[test]
    fn message_kind_top_level_compares_unique() {
        assert_eq!(MessageKind::TopLevel, MessageKind::TopLevel);
        assert_ne!(
            MessageKind::TopLevel,
            MessageKind::ThreadReply {
                root: "x".into(),
                broadcast: false
            }
        );
    }

    #[test]
    fn outbound_clone_preserves_body_and_thread() {
        let parent = MessageRef::top_level("slack", Owner::new("c"), "1");
        let m1 = OutboundMessage::new("body").in_thread(parent);
        let m2 = m1.clone();
        assert_eq!(m1.body, m2.body);
        assert_eq!(m1.thread_parent, m2.thread_parent);
    }

    #[test]
    fn inbound_reaction_carries_all_fields() {
        let from = Participant::new("U1", ParticipantRole::Human);
        let parent = MessageRef::top_level("slack", Owner::new("slack:T1/C1"), "ts:42");
        let when = Utc::now();
        let r = InboundReaction {
            channel: "slack".into(),
            conv: Owner::new("slack:T1/C1"),
            from: from.clone(),
            parent: parent.clone(),
            emoji: "+1".into(),
            added: true,
            timestamp: when,
        };
        assert_eq!(r.channel, "slack");
        assert_eq!(r.from, from);
        assert_eq!(r.parent, parent);
        assert_eq!(r.emoji, "+1");
        assert!(r.added);
        assert_eq!(r.timestamp, when);
    }

    #[test]
    fn inbound_reaction_removal_flag_round_trips() {
        let from = Participant::new("U1", ParticipantRole::Human);
        let parent = MessageRef::top_level("slack", Owner::new("c"), "1");
        let r = InboundReaction {
            channel: "slack".into(),
            conv: Owner::new("c"),
            from,
            parent,
            emoji: "white_check_mark".into(),
            added: false,
            timestamp: Utc::now(),
        };
        assert!(!r.added);
    }
}
