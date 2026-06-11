use async_trait::async_trait;
use crabgent_channel::{
    Channel, ChannelError, ChannelKind, ConvLabel, DirectRole, MessageRef, OutboundMessage,
    Participant, ParticipantId, ParticipantRole, ReadMessage,
};
use crabgent_core::{owner::Owner, subject::Subject};
use matrix_sdk::{
    Room, RoomMemberships,
    deserialized_responses::TimelineEvent,
    room::MessagesOptions,
    ruma::{
        OwnedEventId, UInt, UserId,
        events::{
            relation::Thread,
            room::{
                MediaSource,
                message::{
                    FileMessageEventContent, FormattedBody, ImageMessageEventContent, MessageType,
                    OriginalSyncRoomMessageEvent, Relation, ReplacementMetadata,
                    RoomMessageEventContent,
                },
            },
        },
    },
};

use super::MatrixChannel;
use crate::{
    outbound::{
        CHANNEL_NAME, build_text_content, build_text_content_with_thread, parse_owner_to_room_id,
        parse_recipient_to_user_id,
    },
    outbound_react::build_reaction_content,
};

const READ_LIMIT_MAX: u64 = 100;

#[async_trait]
impl Channel for MatrixChannel {
    fn name(&self) -> &'static str {
        CHANNEL_NAME
    }

    async fn kind(&self, conv: &Owner) -> Result<ChannelKind, ChannelError> {
        let room_id = parse_owner_to_room_id(conv)?;
        self.prefetch_kind(&room_id).await
    }

    /// Best-effort readable labels for a Matrix room.
    ///
    /// `name` is the room's `m.room.name` read from the local SDK state
    /// ([`Room::name`], no network round-trip): `None` when the room is
    /// unknown to the client or carries no name event (an unnamed DM whose
    /// partner is surfaced via the sender display instead). `workspace` is
    /// the homeserver, the server part of the room id, derived purely from
    /// the parsed owner so it is present even on a room-state miss. A
    /// malformed owner or an entirely empty label resolves to `None`.
    async fn conv_display(&self, conv: &Owner) -> Option<ConvLabel> {
        let room_id = parse_owner_to_room_id(conv).ok()?;
        // `Room::name` reads the local SDK room state; the offline test
        // `Client` carries no rooms, so this closure stays uncovered. The
        // pure name/label logic is exercised via `assemble_conv_label` and
        // the workspace-only path via `conv_display` on a missing room.
        let raw_name = self.client.get_room(&room_id).and_then(|room| room.name());
        let workspace = room_id_homeserver(&room_id);
        assemble_conv_label(raw_name, workspace)
    }

    async fn participants(
        &self,
        _ctx: &Subject,
        conv: &Owner,
    ) -> Result<Vec<Participant>, ChannelError> {
        let room_id = parse_owner_to_room_id(conv)?;
        let room = self
            .client
            .get_room(&room_id)
            .ok_or_else(|| ChannelError::ConversationNotFound(room_id.to_string()))?;
        let members = room
            .members(RoomMemberships::ACTIVE)
            .await
            .map_err(ChannelError::adapter)?;
        let mut participants = Vec::with_capacity(members.len().max(1));
        for member in members {
            let role = if member.user_id().as_str() == self.bot_user_id.as_str() {
                ParticipantRole::Bot
            } else {
                ParticipantRole::Human
            };
            let participant =
                Participant::new(ParticipantId::new(member.user_id().to_string()), role);
            participants.push(match member.display_name() {
                Some(name) => participant.with_display_name(name),
                None => participant,
            });
        }
        if participants.is_empty() {
            participants.push(self.bot_participant());
        }
        Ok(participants)
    }

    async fn send(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let room = self.room_for_conv(conv)?;
        let room_id = room.room_id().to_owned();
        let content = build_text_content_with_thread(msg, self.body_cap_bytes)?;
        let response = room.send(content).await.map_err(ChannelError::adapter)?;
        let event_id = response.response.event_id.to_string();
        let conv = Owner::new(format!("{CHANNEL_NAME}:{room_id}"));
        Ok(match msg.thread_parent.as_ref() {
            Some(parent) => {
                let thread_root_id = parent.thread_root_or_id().to_owned();
                MessageRef::thread_reply_broadcast(
                    CHANNEL_NAME,
                    conv,
                    event_id,
                    thread_root_id,
                    false,
                )
            }
            None => MessageRef::top_level(CHANNEL_NAME, conv, event_id),
        })
    }

    async fn react(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        parent: &MessageRef,
        emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        let room = self.room_for_conv(conv)?;
        let content = build_reaction_content(parent, emoji)?;
        let response = room.send(content).await.map_err(ChannelError::adapter)?;
        let event_id = response.response.event_id.to_string();
        let conv = Owner::new(format!("{CHANNEL_NAME}:{}", room.room_id()));
        Ok(MessageRef::top_level(CHANNEL_NAME, conv, event_id))
    }

    async fn edit(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
        new_text: &str,
    ) -> Result<(), ChannelError> {
        let room = self.room_for_conv(conv)?;
        let target_id = event_id_from_message_ref(target, "edit target")?;
        let content = build_text_content(new_text)
            .make_replacement(ReplacementMetadata::new(target_id, None));
        room.send(content).await.map_err(ChannelError::adapter)?;
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
    ) -> Result<(), ChannelError> {
        let room = self.room_for_conv(conv)?;
        let target_id = event_id_from_message_ref(target, "delete target")?;
        room.redact(&target_id, None, None)
            .await
            .map_err(ChannelError::adapter)?;
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
        let room = self.room_for_conv(conv)?;
        let content_type = mime_guess::from_path(filename).first_or_octet_stream();
        let upload = self
            .client
            .media()
            .upload(&content_type, bytes, None)
            .await
            .map_err(ChannelError::adapter)?;
        let (body, formatted) = file_caption_parts(filename, comment);
        let msgtype = if content_type.type_() == mime::IMAGE {
            let mut image =
                ImageMessageEventContent::new(body, MediaSource::Plain(upload.content_uri));
            image.formatted = formatted;
            image.filename = Some(filename.to_owned());
            MessageType::Image(image)
        } else {
            let mut file =
                FileMessageEventContent::new(body, MediaSource::Plain(upload.content_uri));
            file.formatted = formatted;
            file.filename = Some(filename.to_owned());
            MessageType::File(file)
        };
        let mut content = RoomMessageEventContent::new(msgtype);
        apply_thread_relation(&mut content, thread_parent)?;
        let response = room.send(content).await.map_err(ChannelError::adapter)?;
        let event_id = response.response.event_id.to_string();
        let conv = Owner::new(format!("{CHANNEL_NAME}:{}", room.room_id()));
        Ok(match thread_parent {
            Some(parent) => MessageRef::thread_reply_broadcast(
                CHANNEL_NAME,
                conv,
                event_id,
                parent
                    .thread_root
                    .clone()
                    .unwrap_or_else(|| parent.id.clone()),
                false,
            ),
            None => MessageRef::top_level(CHANNEL_NAME, conv, event_id),
        })
    }

    async fn read(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        thread_parent: Option<&MessageRef>,
        limit: usize,
    ) -> Result<Vec<ReadMessage>, ChannelError> {
        let room = self.room_for_conv(conv)?;
        let mut options = MessagesOptions::backward();
        options.limit = limit_to_uint(limit);
        let messages = room
            .messages(options)
            .await
            .map_err(ChannelError::adapter)?;
        Ok(messages
            .chunk
            .iter()
            .filter_map(|event| read_message_from_timeline(conv, thread_parent, event))
            .collect())
    }

    async fn direct_role(&self, conv: &Owner) -> Result<Option<DirectRole>, ChannelError> {
        match self.kind(conv).await? {
            ChannelKind::Direct => Ok(Some(DirectRole::HumanAgent)),
            ChannelKind::Group => Ok(None),
        }
    }

    async fn notify_user(
        &self,
        _ctx: &Subject,
        recipient: &ParticipantId,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let user_id = parse_recipient_to_user_id(recipient)?;
        // Always scan joined_rooms for an existing 2-joined-member room
        // first. matrix-sdk's `Client::get_dm_room` is m.direct-only and
        // would (a) miss rooms the other party created without the bot
        // mirroring m.direct on its side and (b) return a room the
        // recipient has since left. The scan is the single source of
        // truth.
        let room = match self.find_existing_dm_with(&user_id).await {
            Some(room) => room,
            None => self
                .client
                .create_dm(&user_id)
                .await
                .map_err(ChannelError::adapter)?,
        };
        // notify_user delivers a top-level message into a freshly resolved
        // DM. Any thread_parent on the outbound is dropped here so the wire
        // shape matches the top-level MessageRef returned below.
        let body = crabgent_core::text::truncate_bytes_at_boundary(&msg.body, self.body_cap_bytes);
        let content = build_text_content(body);
        let response = room.send(content).await.map_err(ChannelError::adapter)?;
        let event_id = response.response.event_id.to_string();
        let conv = Owner::new(format!("{CHANNEL_NAME}:{}", room.room_id()));
        Ok(MessageRef::top_level(CHANNEL_NAME, conv, event_id))
    }
}

impl MatrixChannel {
    fn room_for_conv(&self, conv: &Owner) -> Result<Room, ChannelError> {
        let room_id = parse_owner_to_room_id(conv)?;
        self.client
            .get_room(&room_id)
            .ok_or_else(|| ChannelError::ConversationNotFound(room_id.to_string()))
    }

    /// Heuristic fallback for `notify_user`: matrix-sdk's `get_dm_room`
    /// only finds rooms tagged via `m.direct` account-data on the bot's
    /// own side. A DM created by the other user (or any room the bot
    /// joined without marking it direct) is invisible to `get_dm_room`,
    /// so a naive caller would always create a fresh DM. This scan
    /// catches that case: any joined room with exactly two joined
    /// members where the other one is `recipient` is treated as the
    /// existing DM. Falls back silently to `None` on transient member
    /// fetch errors so the caller proceeds to `create_dm`.
    async fn find_existing_dm_with(&self, recipient: &UserId) -> Option<Room> {
        for room in self.client.joined_rooms() {
            if room.active_members_count() != 2 {
                continue;
            }
            let Ok(members) = room.members(RoomMemberships::JOIN).await else {
                continue;
            };
            if members.len() == 2 && members.iter().any(|m| m.user_id() == recipient) {
                return Some(room);
            }
        }
        None
    }
}

/// Extract the homeserver (server part) of a Matrix room id as the
/// `workspace` label. `RoomId::server_name` is `None` for the format-v2
/// room ids that omit the server part; in that case there is no readable
/// homeserver to surface and the label stays absent.
fn room_id_homeserver(room_id: &matrix_sdk::ruma::RoomId) -> Option<String> {
    room_id
        .server_name()
        .map(|server| server.as_str().to_owned())
}

/// Assemble the readable [`ConvLabel`] from the raw `m.room.name` and the
/// homeserver. The name is dropped when blank (a redacted/missing name
/// event reads as empty); an entirely empty label collapses to `None` so
/// the inbound tag omits the attrs. Pure so it is unit-testable without
/// live SDK room state, which the `room.name()` lookup itself needs.
fn assemble_conv_label(raw_name: Option<String>, workspace: Option<String>) -> Option<ConvLabel> {
    let name = raw_name.filter(|name| !name.trim().is_empty());
    let label = ConvLabel { name, workspace };
    (!label.is_empty()).then_some(label)
}

fn event_id_from_message_ref(
    message_ref: &MessageRef,
    label: &str,
) -> Result<OwnedEventId, ChannelError> {
    OwnedEventId::try_from(message_ref.id.clone()).map_err(|err| {
        ChannelError::InvalidEnvelope(format!("invalid matrix {label}: {}: {err}", message_ref.id))
    })
}

fn apply_thread_relation(
    content: &mut RoomMessageEventContent,
    thread_parent: Option<&MessageRef>,
) -> Result<(), ChannelError> {
    let Some(parent) = thread_parent else {
        return Ok(());
    };
    let root = parent.thread_root_or_id();
    let root_id = OwnedEventId::try_from(root.to_owned()).map_err(|err| {
        ChannelError::InvalidEnvelope(format!("invalid matrix thread root '{root}': {err}"))
    })?;
    let reply_id = OwnedEventId::try_from(parent.id.clone()).map_err(|err| {
        ChannelError::InvalidEnvelope(format!(
            "invalid matrix reply target '{}': {err}",
            parent.id
        ))
    })?;
    content.relates_to = Some(Relation::Thread(Thread::reply(root_id, reply_id)));
    Ok(())
}

fn file_caption_parts(filename: &str, comment: Option<&str>) -> (String, Option<FormattedBody>) {
    let Some(comment) = comment else {
        return (filename.to_owned(), None);
    };
    let content = build_text_content(comment);
    let MessageType::Text(text) = content.msgtype else {
        return (comment.to_owned(), None);
    };
    (text.body, text.formatted)
}

fn limit_to_uint(limit: usize) -> UInt {
    let limit = u64::try_from(limit).unwrap_or(READ_LIMIT_MAX);
    UInt::new(limit.clamp(1, READ_LIMIT_MAX)).unwrap_or(UInt::MAX)
}

fn read_message_from_timeline(
    conv: &Owner,
    thread_parent: Option<&MessageRef>,
    event: &TimelineEvent,
) -> Option<ReadMessage> {
    let matrix_event = event
        .raw()
        .deserialize_as_unchecked::<OriginalSyncRoomMessageEvent>()
        .ok()?;
    if let Some(parent) = thread_parent {
        let requested_root = parent.thread_root_or_id();
        if message_thread_root(&matrix_event) != Some(requested_root) {
            return None;
        }
    }
    let event_id = matrix_event.event_id.to_string();
    let message_ref = match message_thread_root(&matrix_event) {
        Some(root) => MessageRef::thread_reply_broadcast(
            CHANNEL_NAME,
            conv.clone(),
            event_id,
            root.to_owned(),
            false,
        ),
        None => MessageRef::top_level(CHANNEL_NAME, conv.clone(), event_id),
    };
    Some(ReadMessage {
        message_ref,
        author: ParticipantId::new(matrix_event.sender.to_string()),
        body: body_from_message(&matrix_event.content.msgtype),
        timestamp_unix_ms: event.timestamp().map_or(0, |ts| ts.get().into()),
    })
}

fn message_thread_root(event: &OriginalSyncRoomMessageEvent) -> Option<&str> {
    match &event.content.relates_to {
        Some(Relation::Thread(Thread { event_id, .. })) => Some(event_id.as_str()),
        _ => None,
    }
}

fn body_from_message(msgtype: &MessageType) -> String {
    match msgtype {
        MessageType::Audio(content) => content.body.clone(),
        MessageType::Emote(content) => content.body.clone(),
        MessageType::File(content) => content.body.clone(),
        MessageType::Image(content) => content.body.clone(),
        MessageType::Notice(content) => content.body.clone(),
        MessageType::Text(content) => content.body.clone(),
        MessageType::Video(content) => content.body.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod conv_label_tests {
    use super::{assemble_conv_label, room_id_homeserver};
    use matrix_sdk::ruma::owned_room_id;

    #[test]
    fn homeserver_is_the_room_id_server_part() {
        let room_id = owned_room_id!("!abc:matrix.example.org");
        assert_eq!(
            room_id_homeserver(&room_id).as_deref(),
            Some("matrix.example.org")
        );
    }

    #[test]
    fn full_label_keeps_name_and_workspace() {
        let label =
            assemble_conv_label(Some("Ops Room".to_owned()), Some("example.org".to_owned()))
                .expect("non-empty label");
        assert_eq!(label.name.as_deref(), Some("Ops Room"));
        assert_eq!(label.workspace.as_deref(), Some("example.org"));
    }

    #[test]
    fn blank_name_is_dropped_but_workspace_survives() {
        let label = assemble_conv_label(Some("   ".to_owned()), Some("example.org".to_owned()))
            .expect("workspace-only label");
        assert_eq!(label.name, None);
        assert_eq!(label.workspace.as_deref(), Some("example.org"));
    }

    #[test]
    fn empty_label_collapses_to_none() {
        assert!(assemble_conv_label(None, None).is_none());
        assert!(assemble_conv_label(Some(String::new()), None).is_none());
    }
}
