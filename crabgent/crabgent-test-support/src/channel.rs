//! Recording channel-side doubles: sink, channel, and inbox.
//!
//! These fold the `RecordingSink`/`RecordingChannel`/`CountingInbox`/
//! `RecordingInbox` fixtures that were re-declared across channel and command
//! test modules into one shared surface, each with the introspection accessors
//! the call sites assert against.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::{
    Channel, ChannelError, ChannelInbox, ChannelKind, ChannelSink, InboundEvent, InboundReaction,
    MessageRef, OutboundMessage, Participant, ParticipantId, ParticipantRole, ReadMessage,
};
use crabgent_core::{Owner, Subject};

/// A human participant with id `U1`. Mirrors the per-file
/// `stub_human_participants` helpers.
#[must_use]
pub fn human_participant() -> Participant {
    Participant::new("U1", ParticipantRole::Human)
}

/// A minimal text [`InboundEvent`] on conversation `test:conv` with `body`.
#[must_use]
pub fn inbound_event(body: impl Into<String>) -> InboundEvent {
    let conv = Owner::new("test:conv");
    InboundEvent {
        channel: "test".to_owned(),
        conv: conv.clone(),
        kind: Some(ChannelKind::Group),
        from: Participant::new("u1", ParticipantRole::Human),
        message: MessageRef::top_level("test", conv, "in1"),
        body: body.into(),
        attachments: Vec::new(),
        timestamp: Utc::now(),
    }
}

/// A minimal [`InboundReaction`] (emoji `+1`, added) on `test:conv`.
#[must_use]
pub fn inbound_reaction(emoji: impl Into<String>) -> InboundReaction {
    let conv = Owner::new("test:conv");
    InboundReaction {
        channel: "test".to_owned(),
        conv: conv.clone(),
        from: Participant::new("u1", ParticipantRole::Human),
        parent: MessageRef::top_level("test", conv, "in1"),
        emoji: emoji.into(),
        added: true,
        timestamp: Utc::now(),
    }
}

/// A [`ChannelSink`] that records outbound sends and reactions.
///
/// Replaces the per-file `RecordingSink` doubles; `send` records the body and
/// the thread-parent id, `react` records `(parent_id, emoji)`.
#[derive(Default)]
pub struct RecordingSink {
    sent: Mutex<Vec<String>>,
    thread_parents: Mutex<Vec<Option<String>>>,
    reactions: Mutex<Vec<(String, String)>>,
}

impl RecordingSink {
    /// Construct an empty recording sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bodies of every recorded `send`, in order.
    #[must_use]
    pub fn sent(&self) -> Vec<String> {
        self.sent
            .lock()
            .expect("sent mutex must not be poisoned")
            .clone()
    }

    /// Number of recorded `send` calls.
    #[must_use]
    pub fn sent_count(&self) -> usize {
        self.sent
            .lock()
            .expect("sent mutex must not be poisoned")
            .len()
    }

    /// Thread-parent message ids passed to each `send` (`None` for top-level).
    #[must_use]
    pub fn thread_parents(&self) -> Vec<Option<String>> {
        self.thread_parents
            .lock()
            .expect("thread-parent mutex must not be poisoned")
            .clone()
    }

    /// Recorded `(parent_id, emoji)` pairs from `react`.
    #[must_use]
    pub fn reactions(&self) -> Vec<(String, String)> {
        self.reactions
            .lock()
            .expect("reactions mutex must not be poisoned")
            .clone()
    }
}

#[async_trait]
impl ChannelSink for RecordingSink {
    async fn send(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        self.sent
            .lock()
            .expect("sent mutex must not be poisoned")
            .push(msg.body.clone());
        self.thread_parents
            .lock()
            .expect("thread-parent mutex must not be poisoned")
            .push(msg.thread_parent.as_ref().map(|r| r.id.clone()));
        Ok(MessageRef::top_level("test", conv.clone(), "reply"))
    }

    async fn react(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        parent: &MessageRef,
        emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        self.reactions
            .lock()
            .expect("reactions mutex must not be poisoned")
            .push((parent.id.clone(), emoji.to_owned()));
        Ok(MessageRef::top_level("test", conv.clone(), "react"))
    }
}

/// A full [`Channel`] double that records every outbound operation.
///
/// Replaces the `RecordingChannel` fixtures duplicated across channel tests
/// and `crabgent-channel/src/test_support.rs`, but with `pub` accessors so it
/// can be shared across crate boundaries.
pub struct RecordingChannel {
    name: &'static str,
    kind: ChannelKind,
    message_id: &'static str,
    participants: Vec<Participant>,
    sent: Mutex<Vec<OutboundMessage>>,
    reactions: Mutex<Vec<(MessageRef, String)>>,
    edits: Mutex<Vec<(MessageRef, String)>>,
    deletes: Mutex<Vec<MessageRef>>,
    reads: Mutex<Vec<(Option<MessageRef>, usize)>>,
}

impl Default for RecordingChannel {
    fn default() -> Self {
        Self::new("test", ChannelKind::Group, "stub-id")
    }
}

impl RecordingChannel {
    /// Construct a recording channel with the given name, kind, and the
    /// `MessageRef::id` it stamps on outbound operations.
    #[must_use]
    pub fn new(name: &'static str, kind: ChannelKind, message_id: &'static str) -> Self {
        Self {
            name,
            kind,
            message_id,
            participants: vec![human_participant()],
            sent: Mutex::new(Vec::new()),
            reactions: Mutex::new(Vec::new()),
            edits: Mutex::new(Vec::new()),
            deletes: Mutex::new(Vec::new()),
            reads: Mutex::new(Vec::new()),
        }
    }

    /// Override the participant list returned by [`Channel::participants`].
    #[must_use]
    pub fn with_participants(mut self, participants: Vec<Participant>) -> Self {
        self.participants = participants;
        self
    }

    /// Number of recorded `send` calls.
    #[must_use]
    pub fn sent_count(&self) -> usize {
        self.sent
            .lock()
            .expect("sent mutex must not be poisoned")
            .len()
    }

    /// The most recently sent [`OutboundMessage`], if any.
    #[must_use]
    pub fn last_sent(&self) -> Option<OutboundMessage> {
        self.sent
            .lock()
            .expect("sent mutex must not be poisoned")
            .last()
            .cloned()
    }

    /// Number of recorded `react` calls.
    #[must_use]
    pub fn react_count(&self) -> usize {
        self.reactions
            .lock()
            .expect("reactions mutex must not be poisoned")
            .len()
    }

    /// Number of recorded `edit` calls.
    #[must_use]
    pub fn edit_count(&self) -> usize {
        self.edits
            .lock()
            .expect("edits mutex must not be poisoned")
            .len()
    }

    /// Number of recorded `delete` calls.
    #[must_use]
    pub fn delete_count(&self) -> usize {
        self.deletes
            .lock()
            .expect("deletes mutex must not be poisoned")
            .len()
    }

    /// Number of recorded `read` calls.
    #[must_use]
    pub fn read_count(&self) -> usize {
        self.reads
            .lock()
            .expect("reads mutex must not be poisoned")
            .len()
    }

    fn stamp(&self, conv: &Owner) -> MessageRef {
        MessageRef::top_level(self.name, conv.clone(), self.message_id)
    }
}

#[async_trait]
impl Channel for RecordingChannel {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, ChannelError> {
        Ok(self.kind)
    }

    async fn participants(
        &self,
        _ctx: &Subject,
        _conv: &Owner,
    ) -> Result<Vec<Participant>, ChannelError> {
        Ok(self.participants.clone())
    }

    async fn send(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        self.sent
            .lock()
            .expect("sent mutex must not be poisoned")
            .push(msg.clone());
        Ok(self.stamp(conv))
    }

    async fn react(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        parent: &MessageRef,
        emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        self.reactions
            .lock()
            .expect("reactions mutex must not be poisoned")
            .push((parent.clone(), emoji.to_owned()));
        Ok(self.stamp(conv))
    }

    async fn edit(
        &self,
        _ctx: &Subject,
        _conv: &Owner,
        target: &MessageRef,
        new_text: &str,
    ) -> Result<(), ChannelError> {
        self.edits
            .lock()
            .expect("edits mutex must not be poisoned")
            .push((target.clone(), new_text.to_owned()));
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &Subject,
        _conv: &Owner,
        target: &MessageRef,
    ) -> Result<(), ChannelError> {
        self.deletes
            .lock()
            .expect("deletes mutex must not be poisoned")
            .push(target.clone());
        Ok(())
    }

    async fn read(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        thread_parent: Option<&MessageRef>,
        limit: usize,
    ) -> Result<Vec<ReadMessage>, ChannelError> {
        self.reads
            .lock()
            .expect("reads mutex must not be poisoned")
            .push((thread_parent.cloned(), limit));
        Ok(vec![ReadMessage {
            message_ref: self.stamp(conv),
            author: ParticipantId::new("U1"),
            body: "read body".to_owned(),
            timestamp_unix_ms: 1_700_000_000_000,
        }])
    }
}

/// A [`ChannelInbox`] that counts received events and reactions.
///
/// Replaces the per-file `CountingInbox` doubles.
#[derive(Default)]
pub struct CountingInbox {
    received: AtomicUsize,
    reactions: AtomicUsize,
}

impl CountingInbox {
    /// Construct an empty counting inbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of `receive` calls observed.
    #[must_use]
    pub fn received_count(&self) -> usize {
        self.received.load(Ordering::SeqCst)
    }

    /// Number of `receive_reaction` calls observed.
    #[must_use]
    pub fn reaction_count(&self) -> usize {
        self.reactions.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ChannelInbox for CountingInbox {
    async fn receive(&self, _event: InboundEvent) -> Result<(), ChannelError> {
        self.received.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn receive_reaction(&self, _reaction: InboundReaction) -> Result<(), ChannelError> {
        self.reactions.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A [`ChannelInbox`] that records the body of every received event.
///
/// Replaces the per-file `RecordingInbox` doubles that captured event bodies.
#[derive(Default)]
pub struct RecordingInbox {
    events: Mutex<Vec<String>>,
    reactions: Mutex<Vec<String>>,
}

impl RecordingInbox {
    /// Construct an empty recording inbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bodies of every received event, in order.
    #[must_use]
    pub fn events(&self) -> Vec<String> {
        self.events
            .lock()
            .expect("events mutex must not be poisoned")
            .clone()
    }

    /// Number of received events.
    #[must_use]
    pub fn received_count(&self) -> usize {
        self.events
            .lock()
            .expect("events mutex must not be poisoned")
            .len()
    }

    /// Emojis of every received reaction, in order.
    #[must_use]
    pub fn reactions(&self) -> Vec<String> {
        self.reactions
            .lock()
            .expect("reactions mutex must not be poisoned")
            .clone()
    }
}

#[async_trait]
impl ChannelInbox for RecordingInbox {
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        self.events
            .lock()
            .expect("events mutex must not be poisoned")
            .push(event.body);
        Ok(())
    }

    async fn receive_reaction(&self, reaction: InboundReaction) -> Result<(), ChannelError> {
        self.reactions
            .lock()
            .expect("reactions mutex must not be poisoned")
            .push(reaction.emoji);
        Ok(())
    }
}
