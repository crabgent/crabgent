use std::sync::Mutex;

use async_trait::async_trait;
use crabgent_core::owner::Owner;
use crabgent_core::subject::Subject;

use crate::channel::{Channel, ChannelKind, ReadMessage};
use crate::envelope::{MessageRef, OutboundMessage};
use crate::error::ChannelError;
use crate::participant::{Participant, ParticipantId, ParticipantRole};

type RecordedUpload = (Owner, String, Vec<u8>, Option<String>, Option<MessageRef>);

/// Shared test helper used by channel test modules.
pub fn stub_human_participants() -> Vec<Participant> {
    vec![Participant::new("U1", ParticipantRole::Human)]
}

/// Shared test channel stub for test-only call sites.
pub struct RecordingChannel {
    name: &'static str,
    kind: ChannelKind,
    message_id: &'static str,
    participants: Vec<Participant>,
    sent: Mutex<Vec<(Owner, OutboundMessage)>>,
    reactions: Mutex<Vec<(Owner, MessageRef, String)>>,
    edits: Mutex<Vec<(Owner, MessageRef, String)>>,
    deletes: Mutex<Vec<(Owner, MessageRef)>>,
    uploads: Mutex<Vec<RecordedUpload>>,
    reads: Mutex<Vec<(Owner, Option<MessageRef>, usize)>>,
    notifications: Mutex<Vec<(ParticipantId, OutboundMessage)>>,
}

impl RecordingChannel {
    pub(crate) fn new(name: &'static str, kind: ChannelKind, message_id: &'static str) -> Self {
        Self {
            name,
            kind,
            message_id,
            participants: stub_human_participants(),
            sent: Mutex::new(Vec::new()),
            reactions: Mutex::new(Vec::new()),
            edits: Mutex::new(Vec::new()),
            deletes: Mutex::new(Vec::new()),
            uploads: Mutex::new(Vec::new()),
            reads: Mutex::new(Vec::new()),
            notifications: Mutex::new(Vec::new()),
        }
    }

    #[must_use]
    pub(crate) fn with_participants(mut self, participants: Vec<Participant>) -> Self {
        self.participants = participants;
        self
    }

    pub(crate) fn sent_count(&self) -> usize {
        self.sent
            .lock()
            .expect("mutex should not be poisoned")
            .len()
    }

    pub(crate) fn last_sent(&self) -> Option<OutboundMessage> {
        self.sent
            .lock()
            .expect("mutex should not be poisoned")
            .last()
            .map(|(_, msg)| msg.clone())
    }

    pub(crate) fn react_count(&self) -> usize {
        self.reactions
            .lock()
            .expect("mutex should not be poisoned")
            .len()
    }

    pub(crate) fn last_reaction(&self) -> Option<(MessageRef, String)> {
        self.reactions
            .lock()
            .expect("test result")
            .last()
            .map(|(_, parent, emoji)| (parent.clone(), emoji.clone()))
    }

    pub(crate) fn edit_count(&self) -> usize {
        self.edits
            .lock()
            .expect("mutex should not be poisoned")
            .len()
    }

    pub(crate) fn last_edit(&self) -> Option<(MessageRef, String)> {
        self.edits
            .lock()
            .expect("test result")
            .last()
            .map(|(_, target, text)| (target.clone(), text.clone()))
    }

    pub(crate) fn delete_count(&self) -> usize {
        self.deletes
            .lock()
            .expect("mutex should not be poisoned")
            .len()
    }

    pub(crate) fn upload_count(&self) -> usize {
        self.uploads
            .lock()
            .expect("mutex should not be poisoned")
            .len()
    }

    pub(crate) fn read_count(&self) -> usize {
        self.reads
            .lock()
            .expect("mutex should not be poisoned")
            .len()
    }

    pub(crate) fn notify_user_count(&self) -> usize {
        self.notifications
            .lock()
            .expect("mutex should not be poisoned")
            .len()
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
            .expect("mutex should not be poisoned")
            .push((conv.clone(), msg.clone()));
        Ok(MessageRef::top_level(
            self.name,
            conv.clone(),
            self.message_id,
        ))
    }

    async fn react(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        parent: &MessageRef,
        emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        self.reactions.lock().expect("test result").push((
            conv.clone(),
            parent.clone(),
            emoji.to_owned(),
        ));
        Ok(MessageRef::top_level(
            self.name,
            conv.clone(),
            self.message_id,
        ))
    }

    async fn edit(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
        new_text: &str,
    ) -> Result<(), ChannelError> {
        self.edits.lock().expect("test result").push((
            conv.clone(),
            target.clone(),
            new_text.to_owned(),
        ));
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
    ) -> Result<(), ChannelError> {
        self.deletes
            .lock()
            .expect("test result")
            .push((conv.clone(), target.clone()));
        Ok(())
    }

    async fn upload(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        filename: &str,
        bytes: Vec<u8>,
        comment: Option<&str>,
        thread_parent: Option<&MessageRef>,
    ) -> Result<MessageRef, ChannelError> {
        self.uploads
            .lock()
            .expect("mutex should not be poisoned")
            .push((
                conv.clone(),
                filename.to_owned(),
                bytes,
                comment.map(str::to_owned),
                thread_parent.cloned(),
            ));
        Ok(MessageRef::top_level(
            self.name,
            conv.clone(),
            self.message_id,
        ))
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
            .expect("test result")
            .push((conv.clone(), thread_parent.cloned(), limit));
        Ok(vec![ReadMessage {
            message_ref: MessageRef::top_level(self.name, conv.clone(), self.message_id),
            author: ParticipantId::new("U1"),
            body: "read body".to_owned(),
            timestamp_unix_ms: 1_700_000_000_000,
        }])
    }

    async fn notify_user(
        &self,
        _ctx: &Subject,
        recipient: &ParticipantId,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        self.notifications
            .lock()
            .expect("mutex should not be poisoned")
            .push((recipient.clone(), msg.clone()));
        Ok(MessageRef::top_level(
            self.name,
            Owner::new(format!("{}:notify/{}", self.name, recipient.as_str())),
            self.message_id,
        ))
    }
}
