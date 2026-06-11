//! Channel trait and the `ChannelKind` variant marker.

use async_trait::async_trait;
use crabgent_core::owner::Owner;
use crabgent_core::subject::Subject;

use crate::envelope::{MessageRef, OutboundMessage};
use crate::error::ChannelError;
use crate::participant::{DirectRole, Participant, ParticipantId};

/// The kind of conversation a channel adapter is serving.
///
/// `Group` covers multi-participant rooms (Slack channels, Telegram
/// groups, Signal groups, ...); `Direct` covers 1:1 conversations
/// (DMs, agent-to-agent direct sessions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelKind {
    /// Multi-participant conversation.
    Group,
    /// 1:1 conversation.
    Direct,
}

impl ChannelKind {
    /// Stable string identifier, suitable for `Subject::attrs` or
    /// log fields.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Direct => "direct",
        }
    }

    /// Parse a `ChannelKind` from its `as_str` form.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "group" => Some(Self::Group),
            "direct" => Some(Self::Direct),
            _ => None,
        }
    }
}

impl std::fmt::Display for ChannelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Best-effort, human-readable labels for a conversation.
///
/// Returned by [`Channel::conv_display`] and stamped onto the inbound
/// `Subject` so the kernel inbox can render readable channel context into
/// the `<inbound>` tag. Both fields are optional: an adapter that cannot
/// resolve a name leaves `name` as `None` (the tag omits the attribute),
/// and a constant-per-connection workspace is filled once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConvLabel {
    /// Channel/room name or DM-partner display, when resolvable.
    pub name: Option<String>,
    /// Workspace, team, or homeserver label, when resolvable.
    pub workspace: Option<String>,
}

impl ConvLabel {
    /// `true` when neither `name` nor `workspace` carries a value, so the
    /// caller can skip stamping entirely.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.name.is_none() && self.workspace.is_none()
    }
}

/// A channel adapter (one Slack workspace, one Telegram bot, ...).
///
/// One implementation services many conversations: methods take
/// `conv: &Owner` so the adapter does not need to allocate a new
/// instance per conversation. Conventional `Owner` shapes look like
/// `slack:T123/C456`, `telegram:chat-789`, ... but the format is
/// adapter-defined.
///
/// `participants` is a mandatory method, not optional: an agent must
/// be able to enumerate group members so policies can verify it is
/// authorised before sending. Use the typed helper from `crate::action`
/// to gate it behind `PolicyHook`.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Stable adapter name (e.g. `"slack"`, `"telegram"`,
    /// `"signal"`, `"web"`). Used as the `MessageRef::channel` value
    /// and the prefix for `ChannelRouter` lookups.
    fn name(&self) -> &'static str;

    /// Classify the conversation as Group or Direct.
    async fn kind(&self, conv: &Owner) -> Result<ChannelKind, ChannelError>;

    /// Best-effort, human-readable labels for `conv`.
    ///
    /// Returns `None` when the adapter cannot resolve a readable name
    /// (missing scope, cache miss, or a Direct conversation whose partner
    /// is surfaced via the sender display instead). This call is on the
    /// inbound dispatch path: it must not block, so adapters resolve from a
    /// local cache or pre-warmed map rather than a fresh network round-trip.
    /// A failed lookup never blocks the inbound, it only omits the labels
    /// from the `<inbound>` tag. The default returns `None` so an adapter
    /// without readable labels does not need to override it.
    async fn conv_display(&self, _conv: &Owner) -> Option<ConvLabel> {
        None
    }

    /// Enumerate participants in the conversation. Must reflect the
    /// adapter's authoritative view, not a local cache that may have
    /// missed a kick/leave event: this is the security backstop that
    /// stops an agent from "reading into" groups it left.
    ///
    /// Loads the full participant list. For very large rooms, adapters
    /// may need an internal paginated fetch and then return the merged
    /// snapshot here.
    async fn participants(
        &self,
        ctx: &Subject,
        conv: &Owner,
    ) -> Result<Vec<Participant>, ChannelError>;

    /// Send `msg` to the conversation, returning the resulting
    /// `MessageRef` (top-level or thread-reply depending on
    /// `msg.thread_parent`).
    async fn send(
        &self,
        ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError>;

    /// Post a reaction to `parent` with `emoji`.
    ///
    /// Adapters that support reactions override this method. The
    /// default reports the operation as unsupported.
    async fn react(
        &self,
        ctx: &Subject,
        conv: &Owner,
        parent: &MessageRef,
        emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        let _ = (ctx, conv, parent, emoji);
        Err(ChannelError::Unsupported("react"))
    }

    /// Edit an existing message.
    ///
    /// Adapters that support message updates override this method. The
    /// target `MessageRef::id` is channel-opaque.
    async fn edit(
        &self,
        ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
        new_text: &str,
    ) -> Result<(), ChannelError> {
        let _ = (ctx, conv, target, new_text);
        Err(ChannelError::Unsupported("edit"))
    }

    /// Delete an existing message.
    ///
    /// Adapters that support message deletion override this method. The
    /// target `MessageRef::id` is channel-opaque.
    async fn delete(
        &self,
        ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
    ) -> Result<(), ChannelError> {
        let _ = (ctx, conv, target);
        Err(ChannelError::Unsupported("delete"))
    }

    /// Upload bytes as a file into the conversation.
    ///
    /// `thread_parent` is optional and uses a channel-opaque
    /// `MessageRef::id` when supplied.
    async fn upload(
        &self,
        ctx: &Subject,
        conv: &Owner,
        filename: &str,
        bytes: Vec<u8>,
        comment: Option<&str>,
        thread_parent: Option<&MessageRef>,
    ) -> Result<MessageRef, ChannelError> {
        let _ = (ctx, conv, filename, bytes, comment, thread_parent);
        Err(ChannelError::Unsupported("upload"))
    }

    /// Read recent messages or replies from the conversation.
    ///
    /// `thread_parent` selects replies anchored to an opaque message id.
    async fn read(
        &self,
        ctx: &Subject,
        conv: &Owner,
        thread_parent: Option<&MessageRef>,
        limit: usize,
    ) -> Result<Vec<ReadMessage>, ChannelError> {
        let _ = (ctx, conv, thread_parent, limit);
        Err(ChannelError::Unsupported("read"))
    }

    /// Notify `recipient` out-of-band by opening or reusing a direct
    /// conversation with that participant.
    ///
    /// Unlike `send`, this method does not take a `conv: &Owner`: the
    /// caller addresses a user by `ParticipantId` only, and the adapter
    /// resolves the destination internally (Matrix `create_dm`, Slack
    /// `conversations.open`, Telegram numeric chat id, ...). Adapters
    /// without an out-of-band notification path return the default
    /// `Unsupported("notify_user")` error.
    async fn notify_user(
        &self,
        ctx: &Subject,
        recipient: &ParticipantId,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let _ = (ctx, recipient, msg);
        Err(ChannelError::Unsupported("notify_user"))
    }

    /// Direct-conversation role (human-agent, agent-agent, ...).
    /// Returns `None` for `ChannelKind::Group` conversations. Adapters
    /// may override; the default returns `None` so a Group-only
    /// adapter does not need to think about this method.
    async fn direct_role(&self, _conv: &Owner) -> Result<Option<DirectRole>, ChannelError> {
        Ok(None)
    }
}

/// Adapter-neutral message read result.
#[derive(Debug, Clone)]
pub struct ReadMessage {
    /// Reference to the message in its channel.
    pub message_ref: MessageRef,
    /// Channel-opaque author identifier.
    pub author: ParticipantId,
    /// Plain text body as returned by the adapter.
    pub body: String,
    /// Message timestamp in Unix milliseconds.
    pub timestamp_unix_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::RecordingChannel;

    #[test]
    fn channel_kind_round_trips_string_form() {
        assert_eq!(ChannelKind::Group.as_str(), "group");
        assert_eq!(ChannelKind::Direct.as_str(), "direct");
        assert_eq!(ChannelKind::parse("group"), Some(ChannelKind::Group));
        assert_eq!(ChannelKind::parse("direct"), Some(ChannelKind::Direct));
        assert_eq!(ChannelKind::parse("nope"), None);
    }

    #[test]
    fn channel_kind_display_matches_as_str() {
        assert_eq!(format!("{}", ChannelKind::Group), "group");
        assert_eq!(format!("{}", ChannelKind::Direct), "direct");
    }

    #[test]
    fn channel_kind_copy_and_eq() {
        let a = ChannelKind::Group;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(a, ChannelKind::Direct);
    }

    #[tokio::test]
    async fn default_direct_role_returns_none() {
        let c = RecordingChannel::new("stub", ChannelKind::Group, "stub-id");
        let conv = Owner::new("stub:1");
        let r = c.direct_role(&conv).await.expect("ok");
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn default_conv_display_returns_none() {
        let c = RecordingChannel::new("stub", ChannelKind::Group, "stub-id");
        let conv = Owner::new("stub:1");
        assert!(c.conv_display(&conv).await.is_none());
    }

    #[tokio::test]
    async fn override_conv_display_returns_labels() {
        struct Labeled;

        #[async_trait]
        impl Channel for Labeled {
            fn name(&self) -> &'static str {
                "labeled"
            }

            async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, ChannelError> {
                Ok(ChannelKind::Group)
            }

            async fn conv_display(&self, _conv: &Owner) -> Option<ConvLabel> {
                Some(ConvLabel {
                    name: Some("#platform-ops".to_owned()),
                    workspace: Some("example".to_owned()),
                })
            }

            async fn participants(
                &self,
                _ctx: &Subject,
                _conv: &Owner,
            ) -> Result<Vec<Participant>, ChannelError> {
                Ok(Vec::new())
            }

            async fn send(
                &self,
                _ctx: &Subject,
                _conv: &Owner,
                _msg: &OutboundMessage,
            ) -> Result<MessageRef, ChannelError> {
                Ok(MessageRef::top_level(
                    "labeled",
                    Owner::new("labeled:1"),
                    "id",
                ))
            }
        }

        let label = Labeled.conv_display(&Owner::new("labeled:1")).await;
        assert_eq!(
            label,
            Some(ConvLabel {
                name: Some("#platform-ops".to_owned()),
                workspace: Some("example".to_owned()),
            })
        );
    }

    #[test]
    fn conv_label_is_empty_only_when_both_none() {
        assert!(ConvLabel::default().is_empty());
        assert!(
            !ConvLabel {
                name: Some("x".to_owned()),
                workspace: None,
            }
            .is_empty()
        );
        assert!(
            !ConvLabel {
                name: None,
                workspace: Some("w".to_owned()),
            }
            .is_empty()
        );
    }

    #[tokio::test]
    async fn stub_channel_kind_returns_configured() {
        let c = RecordingChannel::new("stub", ChannelKind::Direct, "stub-id");
        let conv = Owner::new("stub:1");
        let kind = c.kind(&conv).await.expect("ok");
        assert_eq!(kind, ChannelKind::Direct);
    }

    #[tokio::test]
    async fn stub_channel_participants_lists_one() {
        let c = RecordingChannel::new("stub", ChannelKind::Group, "stub-id");
        let s = Subject::new("agent");
        let conv = Owner::new("stub:1");
        let parts = c.participants(&s, &conv).await.expect("ok");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].id.as_str(), "U1");
    }

    #[tokio::test]
    async fn stub_channel_send_records_outbound() {
        let c = RecordingChannel::new("stub", ChannelKind::Direct, "stub-id");
        let s = Subject::new("agent");
        let conv = Owner::new("stub:1");
        let m = OutboundMessage::new("hi");
        let r = c.send(&s, &conv, &m).await.expect("ok");
        assert_eq!(r.id, "stub-id");
        assert_eq!(c.sent_count(), 1);
    }

    #[tokio::test]
    async fn default_react_returns_unsupported() {
        struct Bare;

        #[async_trait]
        impl Channel for Bare {
            fn name(&self) -> &'static str {
                "bare"
            }

            async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, ChannelError> {
                Ok(ChannelKind::Group)
            }

            async fn participants(
                &self,
                _ctx: &Subject,
                _conv: &Owner,
            ) -> Result<Vec<Participant>, ChannelError> {
                Ok(Vec::new())
            }

            async fn send(
                &self,
                _ctx: &Subject,
                _conv: &Owner,
                _msg: &OutboundMessage,
            ) -> Result<MessageRef, ChannelError> {
                Ok(MessageRef::top_level("bare", Owner::new("bare:1"), "id"))
            }
        }

        let c = Bare;
        let s = Subject::new("agent");
        let conv = Owner::new("bare:1");
        let parent = MessageRef::top_level("bare", conv.clone(), "ts:1");
        let err = c
            .react(&s, &conv, &parent, "👀")
            .await
            .expect_err("must fail");
        assert!(matches!(err, ChannelError::Unsupported("react")));
    }

    #[tokio::test]
    async fn default_notify_user_returns_unsupported() {
        struct Bare;

        #[async_trait]
        impl Channel for Bare {
            fn name(&self) -> &'static str {
                "bare"
            }

            async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, ChannelError> {
                Ok(ChannelKind::Direct)
            }

            async fn participants(
                &self,
                _ctx: &Subject,
                _conv: &Owner,
            ) -> Result<Vec<Participant>, ChannelError> {
                Ok(Vec::new())
            }

            async fn send(
                &self,
                _ctx: &Subject,
                _conv: &Owner,
                _msg: &OutboundMessage,
            ) -> Result<MessageRef, ChannelError> {
                Ok(MessageRef::top_level("bare", Owner::new("bare:1"), "id"))
            }
        }

        let c = Bare;
        let s = Subject::new("agent");
        let recipient = ParticipantId::new("U-bare-target");
        let msg = OutboundMessage::new("ping");
        let err = c
            .notify_user(&s, &recipient, &msg)
            .await
            .expect_err("must fail");
        assert!(matches!(err, ChannelError::Unsupported("notify_user")));
    }
}
