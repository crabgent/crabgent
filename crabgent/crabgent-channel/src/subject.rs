//! Channel-aware extension of [`Subject`].
//!
//! Adapters and inbox handlers carry conversation context to the
//! `PolicyHook` by setting attributes on the `Subject`:
//! `channel` (adapter name), `conv` (conversation owner string),
//! `channel_kind` (`group`/`direct`), and optionally
//! `participant_role` (`human`/`bot`/...). Policies can match these
//! string attrs and the typed channel action targets produced by
//! `crate::action`. No new hook type is needed.

use crabgent_core::owner::Owner;
use crabgent_core::subject::Subject;

use crate::channel::{ChannelKind, ConvLabel};
use crate::envelope::{InboundReaction, MessageRef};

mod id;

pub use id::{channel_subject_id, parse_channel_subject_id};

/// Attribute keys that `ChannelSubjectExt` writes into `Subject::attrs`.
pub mod attr_keys {
    /// Adapter name (e.g. `"slack"`).
    pub const CHANNEL: &str = "channel";
    /// Conversation `Owner` as a string.
    pub const CONV: &str = "conv";
    /// `ChannelKind::as_str()` (`"group"` / `"direct"`).
    pub const CHANNEL_KIND: &str = "channel_kind";
    /// Human-readable channel/room name (from `Channel::conv_display`).
    pub const CHANNEL_DISPLAY: &str = "channel_display";
    /// Human-readable workspace/team/homeserver (from `Channel::conv_display`).
    pub const WORKSPACE_DISPLAY: &str = "workspace_display";
    /// Human-readable sender label (from `event.from.display_name`).
    pub const SENDER_DISPLAY: &str = "sender_display";
    /// Participant role (`human` / `bot` / custom).
    pub const PARTICIPANT_ROLE: &str = "participant_role";
    /// Raw per-channel participant id.
    pub const PARTICIPANT_ID: &str = "participant_id";
    /// Inbound message id (channel-opaque) for hooks to rebuild `MessageRef`.
    pub const INBOUND_MSG_ID: &str = "inbound_msg_id";
    /// Inbound message thread root id, set only for thread replies.
    pub const INBOUND_MSG_THREAD_ROOT: &str = "inbound_msg_thread_root";
    /// Inbound message broadcast flag, set only for thread replies.
    pub const INBOUND_MSG_BROADCAST: &str = "inbound_msg_broadcast";
    /// Inbound reaction emoji (channel-opaque short-name or unicode).
    pub const INBOUND_REACTION_EMOJI: &str = "inbound_reaction_emoji";
    /// Inbound reaction target message id (channel-opaque).
    pub const INBOUND_REACTION_TARGET_MSG_ID: &str = "inbound_reaction_target_msg_id";
    /// Inbound reaction add/remove flag: `"true"` on add, `"false"` on remove.
    pub const INBOUND_REACTION_ADDED: &str = "inbound_reaction_added";
}

/// Snapshot of the channel attributes carried by a `Subject`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelAttr<'a> {
    /// Adapter name.
    pub channel: &'a str,
    /// Conversation owner string.
    pub conv: &'a str,
    /// Conversation kind.
    pub kind: ChannelKind,
}

/// Extension trait to set / read channel context on `Subject`.
pub trait ChannelSubjectExt {
    /// Stamp `channel`, `conv`, and `channel_kind` attrs onto the
    /// subject, returning self for chaining.
    #[must_use]
    fn with_channel(self, channel: &str, conv: &Owner, kind: ChannelKind) -> Self;

    /// Stamp the `participant_role` attr onto the subject.
    #[must_use]
    fn with_participant_role(self, role: &str) -> Self;

    /// Stamp the inbound message reference attrs onto the subject.
    #[must_use]
    fn with_inbound_message_ref(self, m: &MessageRef) -> Self;

    /// Stamp the inbound reaction attrs onto the subject.
    ///
    /// Encodes `emoji`, `target_msg_id` (from `reaction.parent.id`) and
    /// the boolean `added` flag as string `"true"` / `"false"`.
    #[must_use]
    fn with_inbound_reaction(self, r: &InboundReaction) -> Self;

    /// Stamp the human-readable conversation labels onto the subject.
    ///
    /// Each component of `label` is written only when present, so an empty
    /// label leaves the subject unchanged. Mirrors `Channel::conv_display`.
    #[must_use]
    fn with_conv_display(self, label: &ConvLabel) -> Self;

    /// Stamp the human-readable sender label onto the subject.
    ///
    /// `display` comes from `event.from.display_name`; a `None` leaves the
    /// subject unchanged.
    #[must_use]
    fn with_sender_display(self, display: Option<&str>) -> Self;

    /// Return the channel attribute snapshot if all three of
    /// `channel`, `conv`, and `channel_kind` are set and `channel_kind`
    /// parses cleanly.
    fn channel(&self) -> Option<ChannelAttr<'_>>;

    /// Return the participant-role attribute, if set.
    fn participant_role(&self) -> Option<&str>;

    /// Rebuild the inbound message reference, if its attrs are present.
    fn inbound_message_ref(&self) -> Option<MessageRef>;

    /// Snapshot of the inbound reaction context, if its attrs are present.
    fn inbound_reaction(&self) -> Option<InboundReactionAttr<'_>>;
}

/// Snapshot of the reaction attributes carried by a `Subject`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundReactionAttr<'a> {
    /// Emoji string.
    pub emoji: &'a str,
    /// Target message id (channel-opaque).
    pub target_msg_id: &'a str,
    /// `true` on add, `false` on remove.
    pub added: bool,
}

impl ChannelSubjectExt for Subject {
    fn with_channel(self, channel: &str, conv: &Owner, kind: ChannelKind) -> Self {
        self.with_attr(attr_keys::CHANNEL, channel)
            .with_attr(attr_keys::CONV, conv.as_str())
            .with_attr(attr_keys::CHANNEL_KIND, kind.as_str())
    }

    fn with_participant_role(self, role: &str) -> Self {
        self.with_attr(attr_keys::PARTICIPANT_ROLE, role)
    }

    fn with_inbound_message_ref(self, m: &MessageRef) -> Self {
        let mut subject = self.with_attr(attr_keys::INBOUND_MSG_ID, m.id.as_str());
        if let Some(root) = m.thread_root() {
            subject = subject
                .with_attr(attr_keys::INBOUND_MSG_THREAD_ROOT, root)
                .with_attr(attr_keys::INBOUND_MSG_BROADCAST, m.broadcast().to_string());
        }
        subject
    }

    fn with_inbound_reaction(self, r: &InboundReaction) -> Self {
        self.with_attr(attr_keys::INBOUND_REACTION_EMOJI, r.emoji.as_str())
            .with_attr(
                attr_keys::INBOUND_REACTION_TARGET_MSG_ID,
                r.parent.id.as_str(),
            )
            .with_attr(attr_keys::INBOUND_REACTION_ADDED, r.added.to_string())
    }

    fn with_conv_display(mut self, label: &ConvLabel) -> Self {
        if let Some(name) = label.name.as_deref() {
            self = self.with_attr(attr_keys::CHANNEL_DISPLAY, name);
        }
        if let Some(workspace) = label.workspace.as_deref() {
            self = self.with_attr(attr_keys::WORKSPACE_DISPLAY, workspace);
        }
        self
    }

    fn with_sender_display(self, display: Option<&str>) -> Self {
        match display {
            Some(name) => self.with_attr(attr_keys::SENDER_DISPLAY, name),
            None => self,
        }
    }

    fn channel(&self) -> Option<ChannelAttr<'_>> {
        let channel = self.attr(attr_keys::CHANNEL)?;
        let conv = self.attr(attr_keys::CONV)?;
        let kind = self
            .attr(attr_keys::CHANNEL_KIND)
            .and_then(ChannelKind::parse)?;
        Some(ChannelAttr {
            channel,
            conv,
            kind,
        })
    }

    fn participant_role(&self) -> Option<&str> {
        self.attr(attr_keys::PARTICIPANT_ROLE)
    }

    fn inbound_message_ref(&self) -> Option<MessageRef> {
        let channel = self.attr(attr_keys::CHANNEL)?;
        let conv = Owner::new(self.attr(attr_keys::CONV)?);
        let id = self.attr(attr_keys::INBOUND_MSG_ID)?;
        let root = self.attr(attr_keys::INBOUND_MSG_THREAD_ROOT);
        let broadcast = self.attr(attr_keys::INBOUND_MSG_BROADCAST) == Some("true");
        Some(match root {
            Some(root) => MessageRef::thread_reply_broadcast(channel, conv, id, root, broadcast),
            None => MessageRef::top_level(channel, conv, id),
        })
    }

    fn inbound_reaction(&self) -> Option<InboundReactionAttr<'_>> {
        let emoji = self.attr(attr_keys::INBOUND_REACTION_EMOJI)?;
        let target_msg_id = self.attr(attr_keys::INBOUND_REACTION_TARGET_MSG_ID)?;
        let added = self.attr(attr_keys::INBOUND_REACTION_ADDED) == Some("true");
        Some(InboundReactionAttr {
            emoji,
            target_msg_id,
            added,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_channel_sets_three_attrs() {
        let s = Subject::new("agent").with_channel(
            "slack",
            &Owner::new("slack:T1/C1"),
            ChannelKind::Group,
        );
        assert_eq!(s.attr("channel"), Some("slack"));
        assert_eq!(s.attr("conv"), Some("slack:T1/C1"));
        assert_eq!(s.attr("channel_kind"), Some("group"));
    }

    #[test]
    fn with_participant_role_sets_attr() {
        let s = Subject::new("agent").with_participant_role("bot");
        assert_eq!(s.attr("participant_role"), Some("bot"));
        assert_eq!(s.participant_role(), Some("bot"));
    }

    #[test]
    fn participant_id_attr_key_is_stable() {
        assert_eq!(attr_keys::PARTICIPANT_ID, "participant_id");
    }

    #[test]
    fn with_conv_display_stamps_present_components_only() {
        let full = Subject::new("u").with_conv_display(&ConvLabel {
            name: Some("#platform-ops".to_owned()),
            workspace: Some("example".to_owned()),
        });
        assert_eq!(full.attr("channel_display"), Some("#platform-ops"));
        assert_eq!(full.attr("workspace_display"), Some("example"));

        let name_only = Subject::new("u").with_conv_display(&ConvLabel {
            name: Some("#ops".to_owned()),
            workspace: None,
        });
        assert_eq!(name_only.attr("channel_display"), Some("#ops"));
        assert_eq!(name_only.attr("workspace_display"), None);

        let empty = Subject::new("u").with_conv_display(&ConvLabel::default());
        assert_eq!(empty.attr("channel_display"), None);
        assert_eq!(empty.attr("workspace_display"), None);
    }

    #[test]
    fn with_sender_display_stamps_only_when_present() {
        let some = Subject::new("u").with_sender_display(Some("Alice"));
        assert_eq!(some.attr("sender_display"), Some("Alice"));

        let none = Subject::new("u").with_sender_display(None);
        assert_eq!(none.attr("sender_display"), None);
    }

    #[test]
    fn channel_returns_snapshot_when_all_set() {
        let s = Subject::new("u").with_channel(
            "slack",
            &Owner::new("slack:T1/D1"),
            ChannelKind::Direct,
        );
        let snap = s.channel().expect("channel attrs present");
        assert_eq!(snap.channel, "slack");
        assert_eq!(snap.conv, "slack:T1/D1");
        assert_eq!(snap.kind, ChannelKind::Direct);
    }

    #[test]
    fn channel_returns_none_when_channel_missing() {
        let s = Subject::new("u")
            .with_attr("conv", "x")
            .with_attr("channel_kind", "group");
        assert!(s.channel().is_none());
    }

    #[test]
    fn channel_returns_none_when_conv_missing() {
        let s = Subject::new("u")
            .with_attr("channel", "slack")
            .with_attr("channel_kind", "group");
        assert!(s.channel().is_none());
    }

    #[test]
    fn channel_returns_none_when_kind_unparseable() {
        let s = Subject::new("u")
            .with_attr("channel", "slack")
            .with_attr("conv", "c")
            .with_attr("channel_kind", "nonsense");
        assert!(s.channel().is_none());
    }

    #[test]
    fn participant_role_returns_none_when_missing() {
        let s = Subject::new("u");
        assert!(s.participant_role().is_none());
    }

    #[test]
    fn with_channel_chain_can_combine_with_participant_role() {
        let s = Subject::new("agent")
            .with_channel("tg", &Owner::new("tg:42"), ChannelKind::Direct)
            .with_participant_role("bot");
        let snap = s.channel().expect("channel set");
        assert_eq!(snap.channel, "tg");
        assert_eq!(s.participant_role(), Some("bot"));
    }

    #[test]
    fn channel_attr_equality_holds() {
        let a = ChannelAttr {
            channel: "slack",
            conv: "c",
            kind: ChannelKind::Group,
        };
        let b = ChannelAttr {
            channel: "slack",
            conv: "c",
            kind: ChannelKind::Group,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn channel_attr_keys_are_stable_strings() {
        // Smoke-test the constants stay reachable + match documented
        // values. Consumer-side policies grep on these.
        assert_eq!(attr_keys::CHANNEL, "channel");
        assert_eq!(attr_keys::CONV, "conv");
        assert_eq!(attr_keys::CHANNEL_KIND, "channel_kind");
        assert_eq!(attr_keys::CHANNEL_DISPLAY, "channel_display");
        assert_eq!(attr_keys::WORKSPACE_DISPLAY, "workspace_display");
        assert_eq!(attr_keys::SENDER_DISPLAY, "sender_display");
        assert_eq!(attr_keys::PARTICIPANT_ROLE, "participant_role");
        assert_eq!(attr_keys::INBOUND_MSG_ID, "inbound_msg_id");
        assert_eq!(
            attr_keys::INBOUND_MSG_THREAD_ROOT,
            "inbound_msg_thread_root"
        );
        assert_eq!(attr_keys::INBOUND_MSG_BROADCAST, "inbound_msg_broadcast");
    }

    #[test]
    fn inbound_message_ref_round_trip_top_level() {
        let conv = Owner::new("slack:T1/C1");
        let message = MessageRef::top_level("slack", conv.clone(), "ts:1");
        let subject = Subject::new("agent")
            .with_channel("slack", &conv, ChannelKind::Group)
            .with_inbound_message_ref(&message);
        assert_eq!(subject.inbound_message_ref(), Some(message));
    }

    #[test]
    fn inbound_message_ref_round_trip_thread_reply() {
        let conv = Owner::new("slack:T1/C1");
        let message =
            MessageRef::thread_reply_broadcast("slack", conv.clone(), "ts:2", "ts:1", true);
        let subject = Subject::new("agent")
            .with_channel("slack", &conv, ChannelKind::Group)
            .with_inbound_message_ref(&message);
        assert_eq!(subject.inbound_message_ref(), Some(message));
    }

    #[test]
    fn inbound_message_ref_returns_none_without_msg_id() {
        let subject = Subject::new("agent").with_channel(
            "slack",
            &Owner::new("slack:T1/C1"),
            ChannelKind::Group,
        );
        assert!(subject.inbound_message_ref().is_none());
    }

    #[test]
    fn inbound_message_ref_returns_none_without_channel() {
        let subject = Subject::new("agent")
            .with_attr(attr_keys::CONV, "slack:T1/C1")
            .with_attr(attr_keys::INBOUND_MSG_ID, "ts:1");
        assert!(subject.inbound_message_ref().is_none());
    }

    #[test]
    fn inbound_reaction_round_trip_added() {
        use crate::envelope::InboundReaction;
        use crate::participant::{Participant, ParticipantRole};
        use chrono::Utc;
        let conv = Owner::new("slack:T1/C1");
        let parent = MessageRef::top_level("slack", conv.clone(), "ts:1");
        let r = InboundReaction {
            channel: "slack".into(),
            conv: conv.clone(),
            from: Participant::new("U1", ParticipantRole::Human),
            parent,
            emoji: "+1".into(),
            added: true,
            timestamp: Utc::now(),
        };
        let subject = Subject::new("agent")
            .with_channel("slack", &conv, ChannelKind::Group)
            .with_inbound_reaction(&r);
        let snap = subject.inbound_reaction().expect("reaction attrs present");
        assert_eq!(snap.emoji, "+1");
        assert_eq!(snap.target_msg_id, "ts:1");
        assert!(snap.added);
    }

    #[test]
    fn inbound_reaction_round_trip_removed() {
        use crate::envelope::InboundReaction;
        use crate::participant::{Participant, ParticipantRole};
        use chrono::Utc;
        let conv = Owner::new("slack:T1/C1");
        let parent = MessageRef::top_level("slack", conv.clone(), "ts:7");
        let r = InboundReaction {
            channel: "slack".into(),
            conv: conv.clone(),
            from: Participant::new("U1", ParticipantRole::Human),
            parent,
            emoji: "white_check_mark".into(),
            added: false,
            timestamp: Utc::now(),
        };
        let subject = Subject::new("agent")
            .with_channel("slack", &conv, ChannelKind::Direct)
            .with_inbound_reaction(&r);
        let snap = subject.inbound_reaction().expect("reaction attrs present");
        assert_eq!(snap.emoji, "white_check_mark");
        assert_eq!(snap.target_msg_id, "ts:7");
        assert!(!snap.added);
    }

    #[test]
    fn inbound_reaction_returns_none_when_emoji_missing() {
        let subject = Subject::new("agent")
            .with_attr(attr_keys::INBOUND_REACTION_TARGET_MSG_ID, "ts:1")
            .with_attr(attr_keys::INBOUND_REACTION_ADDED, "true");
        assert!(subject.inbound_reaction().is_none());
    }

    #[test]
    fn inbound_reaction_returns_none_when_target_missing() {
        let subject = Subject::new("agent")
            .with_attr(attr_keys::INBOUND_REACTION_EMOJI, "+1")
            .with_attr(attr_keys::INBOUND_REACTION_ADDED, "true");
        assert!(subject.inbound_reaction().is_none());
    }

    #[test]
    fn inbound_reaction_attr_keys_are_stable_strings() {
        assert_eq!(attr_keys::INBOUND_REACTION_EMOJI, "inbound_reaction_emoji");
        assert_eq!(
            attr_keys::INBOUND_REACTION_TARGET_MSG_ID,
            "inbound_reaction_target_msg_id"
        );
        assert_eq!(attr_keys::INBOUND_REACTION_ADDED, "inbound_reaction_added");
    }
}
