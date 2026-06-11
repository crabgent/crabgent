//! Shared inbound event assembly for channel adapters.
//!
//! Adapters still decode their native wire format locally. This module owns the
//! adapter-neutral invariants that must stay identical across inbound paths:
//! byte-cap before sanitize, `MessageRef` construction, participant metadata,
//! and typed attachment propagation.

use chrono::{DateTime, Utc};
use crabgent_core::{ContentBlock, owner::Owner};

use crate::channel::ChannelKind;
use crate::envelope::{InboundEvent, MessageRef};
use crate::error::ChannelError;
use crate::inbox::{check_inbound_size, sanitize_for_prompt};
use crate::participant::{Participant, ParticipantId, ParticipantRole};

/// Adapter-attested sender metadata for an inbound event.
#[derive(Debug, Clone)]
pub struct InboundParticipant {
    id: ParticipantId,
    role: ParticipantRole,
    display_name: Option<String>,
}

impl InboundParticipant {
    /// Build a participant spec from the adapter's sender id and role.
    pub fn new(id: impl Into<ParticipantId>, role: ParticipantRole) -> Self {
        Self {
            id: id.into(),
            role,
            display_name: None,
        }
    }

    /// Attach an adapter-resolved display name.
    #[must_use]
    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }

    fn into_participant(self) -> Participant {
        let participant = Participant::new(self.id, self.role);
        match self.display_name {
            Some(display_name) => participant.with_display_name(display_name),
            None => participant,
        }
    }
}

/// Raw inbound body that has passed the channel byte cap.
#[derive(Debug, Clone)]
pub struct InboundBody(String);

impl InboundBody {
    /// Validate the raw body before it reaches prompt-bound assembly.
    pub fn new(body: impl Into<String>) -> Result<Self, ChannelError> {
        let body = body.into();
        check_inbound_size(&body)?;
        Ok(Self(body))
    }

    fn into_sanitized(self) -> String {
        sanitize_for_prompt(&self.0)
    }
}

/// Builder for an adapter-neutral [`InboundEvent`].
#[derive(Debug)]
pub struct InboundEventBuilder {
    channel: String,
    conv: Owner,
    kind: Option<ChannelKind>,
    from: InboundParticipant,
    message_id: String,
    thread_root: Option<String>,
    broadcast: bool,
    body: InboundBody,
    attachments: Vec<ContentBlock>,
    timestamp: DateTime<Utc>,
}

impl InboundEventBuilder {
    /// Start assembling an inbound event from decoded adapter fields.
    pub fn new(
        channel: impl Into<String>,
        conv: Owner,
        message_id: impl Into<String>,
        from: InboundParticipant,
        body: InboundBody,
        timestamp: DateTime<Utc>,
    ) -> Self {
        Self {
            channel: channel.into(),
            conv,
            kind: None,
            from,
            message_id: message_id.into(),
            thread_root: None,
            broadcast: false,
            body,
            attachments: Vec::new(),
            timestamp,
        }
    }

    /// Set the resolved channel kind when the adapter knows it.
    #[must_use]
    pub const fn kind(mut self, kind: ChannelKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Set an optional resolved channel kind.
    #[must_use]
    pub const fn maybe_kind(mut self, kind: Option<ChannelKind>) -> Self {
        self.kind = kind;
        self
    }

    /// Mark the inbound message as a reply inside a thread.
    #[must_use]
    pub fn thread_root(mut self, root: impl Into<String>) -> Self {
        self.thread_root = Some(root.into());
        self
    }

    /// Control whether the thread reply is broadcast in the parent timeline.
    #[must_use]
    pub const fn broadcast(mut self, broadcast: bool) -> Self {
        self.broadcast = broadcast;
        self
    }

    /// Attach already validated or fallback attachment blocks.
    #[must_use]
    pub fn attachments(mut self, attachments: Vec<ContentBlock>) -> Self {
        self.attachments = attachments;
        self
    }

    /// Build the event, sanitizing only after [`InboundBody`] validation.
    #[must_use]
    pub fn build(self) -> InboundEvent {
        let body = self.body.into_sanitized();
        let message = match self.thread_root {
            Some(root) => MessageRef::thread_reply_broadcast(
                self.channel.clone(),
                self.conv.clone(),
                self.message_id,
                root,
                self.broadcast,
            ),
            None => MessageRef::top_level(self.channel.clone(), self.conv.clone(), self.message_id),
        };

        InboundEvent {
            channel: self.channel,
            conv: self.conv,
            kind: self.kind,
            from: self.from.into_participant(),
            message,
            body,
            attachments: self.attachments,
            timestamp: self.timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::{AudioPayload, ImagePayload};

    #[test]
    fn build_sanitizes_body_after_size_check() {
        let event = InboundEventBuilder::new(
            "slack",
            Owner::new("slack:T1/C1"),
            "ts:1",
            InboundParticipant::new("U1", ParticipantRole::Human),
            InboundBody::new("<script>\u{200B}&").expect("valid body"),
            Utc::now(),
        )
        .kind(ChannelKind::Group)
        .build();

        assert_eq!(event.body, "&lt;script&gt;&amp;");
        assert_eq!(event.kind, Some(ChannelKind::Group));
    }

    #[test]
    fn build_returns_typed_error_for_oversized_body() {
        let err = InboundBody::new("a".repeat(crate::INBOUND_BODY_MAX_BYTES + 1))
            .expect_err("oversized body should be rejected");

        assert!(matches!(
            err,
            ChannelError::InboundTooLarge { observed, max }
                if observed == crate::INBOUND_BODY_MAX_BYTES + 1
                    && max == crate::INBOUND_BODY_MAX_BYTES
        ));
    }

    #[test]
    fn build_assembles_thread_ref_and_participant_display_name() {
        let event = InboundEventBuilder::new(
            "matrix",
            Owner::new("matrix:!room:example.org"),
            "$reply:example.org",
            InboundParticipant::new("@alice:example.org", ParticipantRole::Human)
                .with_display_name("Alice"),
            InboundBody::new("hi").expect("valid body"),
            Utc::now(),
        )
        .maybe_kind(Some(ChannelKind::Group))
        .thread_root("$root:example.org")
        .broadcast(false)
        .build();

        assert_eq!(event.message.thread_root(), Some("$root:example.org"));
        assert_eq!(event.from.display_name.as_deref(), Some("Alice"));
    }

    #[test]
    fn build_propagates_typed_media_attachments() {
        let image = ContentBlock::Image(
            ImagePayload::new(b"iVBOR".to_vec(), "image/png").expect("valid image payload"),
        );
        let audio = ContentBlock::Audio(
            AudioPayload::new(
                b"OggS".to_vec(),
                "audio/ogg".to_owned(),
                Some("voice.ogg".into()),
            )
            .expect("valid audio payload"),
        );

        let event = InboundEventBuilder::new(
            "slack",
            Owner::new("slack:T1/D1"),
            "ts:1",
            InboundParticipant::new("U1", ParticipantRole::Human),
            InboundBody::new("media").expect("valid body"),
            Utc::now(),
        )
        .attachments(vec![image, audio])
        .build();

        assert!(matches!(event.attachments[0], ContentBlock::Image(_)));
        assert!(matches!(event.attachments[1], ContentBlock::Audio(_)));
    }
}
