//! Inbound Matrix event mapping.

use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use crabgent_channel::{
    AudioValidator, ChannelKind, ImageStore, ImageValidator, InboundBody, InboundEvent,
    InboundEventBuilder, InboundParticipant, InboundReaction, MessageRef, Participant,
    ParticipantId, ParticipantRole, assemble_audio_attachment, assemble_image_attachment,
    image_download_size_fallback, image_processing_fallback,
};
use crabgent_core::message::ContentBlock;
use crabgent_core::owner::Owner;
use crabgent_log::{debug, warn};
use matrix_sdk::{
    Client,
    deserialized_responses::TimelineEvent,
    ruma::{
        MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedMxcUri, OwnedRoomId, OwnedUserId,
        events::{
            OriginalSyncMessageLikeEvent,
            reaction::ReactionEventContent,
            relation::Thread,
            room::{
                MediaSource,
                message::{MessageType, OriginalSyncRoomMessageEvent, Relation},
            },
        },
    },
};
use serde::Deserialize;

use crate::{
    audio_download::{AudioDownloadError, download_matrix_audio},
    image_download::{ImageDownloadError, download_matrix_image},
    outbound::CHANNEL_NAME,
    reaction_tracker::{ReactionTracker, TrackedReaction},
};

pub(crate) struct InboundMediaClients<'a> {
    pub(crate) matrix_client: &'a Client,
    pub(crate) image_http_client: &'a reqwest::Client,
    pub(crate) image_store: Option<&'a Arc<dyn ImageStore>>,
    pub(crate) image_validator: Option<&'a ImageValidator>,
    pub(crate) audio_http_client: &'a reqwest::Client,
    pub(crate) audio_validator: Option<&'a AudioValidator>,
    pub(crate) access_token: Option<&'a str>,
}

/// Map a Matrix timeline event to a crabgent inbound event.
pub(crate) async fn timeline_event_to_inbound(
    room_id: &OwnedRoomId,
    event: &TimelineEvent,
    bot_user_id: &OwnedUserId,
    kind: Option<ChannelKind>,
    media: &InboundMediaClients<'_>,
) -> Option<InboundEvent> {
    let matrix_event = event
        .raw()
        .deserialize_as_unchecked::<OriginalSyncRoomMessageEvent>()
        .ok()?;
    original_message_to_inbound(
        room_id,
        &matrix_event,
        event.timestamp(),
        bot_user_id,
        kind,
        media,
    )
    .await
}

async fn original_message_to_inbound(
    room_id: &OwnedRoomId,
    event: &OriginalSyncRoomMessageEvent,
    observed_ts: Option<MilliSecondsSinceUnixEpoch>,
    bot_user_id: &OwnedUserId,
    kind: Option<ChannelKind>,
    media: &InboundMediaClients<'_>,
) -> Option<InboundEvent> {
    if event.sender == *bot_user_id {
        return None;
    }
    let (body, attachments) = match &event.content.msgtype {
        MessageType::Text(content) => (validated_body(&content.body, "text")?, vec![]),
        MessageType::Image(content) => {
            let MediaSource::Plain(source) = &content.source else {
                return None;
            };
            let raw_caption = content
                .caption()
                .map_or_else(|| content.body.clone(), ToString::to_string);
            let body = validated_body(&raw_caption, "image caption")?;
            let image = match (media.image_store, media.image_validator) {
                (Some(store), Some(validator)) => {
                    vec![
                        build_matrix_image_attachment(
                            media.matrix_client,
                            media.image_http_client,
                            store.as_ref(),
                            validator,
                            source,
                            media.access_token,
                        )
                        .await,
                    ]
                }
                _ => vec![],
            };
            (body, image)
        }
        MessageType::Audio(content) => {
            let MediaSource::Plain(source) = &content.source else {
                return None;
            };
            let body = validated_body(&content.body, "audio body")?;
            let audio = match media.audio_validator {
                Some(validator) => {
                    match build_matrix_audio_attachment(
                        media.matrix_client,
                        media.audio_http_client,
                        validator,
                        source,
                        media.access_token,
                        content.body.clone(),
                    )
                    .await
                    {
                        Ok(block) => vec![block],
                        Err(error) => {
                            // Drop on validation failure (debug log only); slack rejected()-Text pattern intentionally not used here.
                            debug!(
                                %error,
                                event_id = event.event_id.as_str(),
                                "matrix audio attachment skipped"
                            );
                            vec![]
                        }
                    }
                }
                None => vec![],
            };
            (body, audio)
        }
        _ => return None,
    };

    build_matrix_inbound_event(room_id, event, observed_ts, kind, body, attachments)
}

/// Assemble the final `InboundEvent` from a resolved raw body and attachments.
fn build_matrix_inbound_event(
    room_id: &OwnedRoomId,
    event: &OriginalSyncRoomMessageEvent,
    observed_ts: Option<MilliSecondsSinceUnixEpoch>,
    kind: Option<ChannelKind>,
    body: InboundBody,
    attachments: Vec<ContentBlock>,
) -> Option<InboundEvent> {
    let conv = Owner::new(format!("{CHANNEL_NAME}:{room_id}"));
    let event_id = event.event_id.to_string();
    let participant = InboundParticipant::new(event.sender.to_string(), ParticipantRole::Human);
    let mut builder = InboundEventBuilder::new(
        CHANNEL_NAME,
        conv,
        event_id,
        participant,
        body,
        matrix_timestamp_to_utc(observed_ts),
    )
    .maybe_kind(kind)
    .attachments(attachments);
    if let Some(root) = thread_root(event.content.relates_to.as_ref()) {
        builder = builder.thread_root(root);
    }
    Some(builder.build())
}

fn validated_body(body: &str, label: &str) -> Option<InboundBody> {
    match InboundBody::new(body) {
        Ok(body) => Some(body),
        Err(err) => {
            warn!(%err, body = label, "dropping oversized matrix inbound text");
            None
        }
    }
}

async fn build_matrix_image_attachment(
    matrix_client: &Client,
    image_http_client: &reqwest::Client,
    store: &dyn ImageStore,
    validator: &ImageValidator,
    source: &OwnedMxcUri,
    access_token: Option<&str>,
) -> ContentBlock {
    let (bytes, declared_mime) =
        match download_matrix_image(image_http_client, matrix_client, source, access_token).await {
            Ok(download) => download,
            Err(error) => {
                debug!(%error, "matrix image download failed");
                return image_download_fallback(&error);
            }
        };
    assemble_image_attachment(bytes, &declared_mime, store, validator, "matrix image").await
}

fn image_download_fallback(error: &ImageDownloadError) -> ContentBlock {
    match error {
        ImageDownloadError::Size => image_download_size_fallback(),
        ImageDownloadError::Auth
        | ImageDownloadError::Network
        | ImageDownloadError::Mime
        | ImageDownloadError::Storage => image_processing_fallback(),
    }
}

async fn build_matrix_audio_attachment(
    matrix_client: &Client,
    http_client: &reqwest::Client,
    validator: &AudioValidator,
    source: &OwnedMxcUri,
    access_token: Option<&str>,
    filename: String,
) -> Result<ContentBlock, AudioDownloadError> {
    let (bytes, declared_mime) =
        download_matrix_audio(http_client, matrix_client, source, access_token).await?;
    assemble_audio_attachment(
        &bytes,
        declared_mime,
        Some(filename),
        validator,
        "matrix audio",
    )
    .map_err(|_error| AudioDownloadError::Mime)
}

fn thread_root(
    relation: Option<
        &Relation<matrix_sdk::ruma::events::room::message::RoomMessageEventContentWithoutRelation>,
    >,
) -> Option<String> {
    match relation {
        Some(Relation::Thread(Thread { event_id, .. })) => Some(event_id.to_string()),
        _ => None,
    }
}

fn matrix_timestamp_to_utc(ts: Option<MilliSecondsSinceUnixEpoch>) -> DateTime<Utc> {
    let Some(ts) = ts else {
        return Utc::now();
    };
    Utc.timestamp_millis_opt(ts.get().into())
        .single()
        .unwrap_or_else(Utc::now)
}

/// Byte cap for an inbound reaction key (emoji or short-name).
///
/// A malicious homeserver can put an arbitrarily large string in
/// `relates_to.key`. That value flows into `InboundReaction.emoji` and,
/// unlike the message body, is not covered by `INBOUND_BODY_MAX_BYTES`
/// before it reaches the synthesized reaction event. Cap it here at the
/// trust boundary so the downstream prompt body stays bounded.
const REACTION_KEY_MAX_BYTES: usize = 256;

/// Truncate a reaction key to [`REACTION_KEY_MAX_BYTES`] on a char
/// boundary. Honest emoji and short-names are far below the cap, so this
/// only ever trims hostile oversized keys.
fn cap_reaction_key(key: &str) -> String {
    crabgent_core::text::truncate_bytes_at_boundary(key, REACTION_KEY_MAX_BYTES).to_owned()
}

/// Map a Matrix `m.reaction` timeline event to a crabgent inbound
/// reaction. Records the reaction `event_id` in `tracker` so a later
/// redaction can be turned into an `added: false` event.
pub(crate) fn timeline_event_to_inbound_reaction(
    room_id: &OwnedRoomId,
    event: &TimelineEvent,
    bot_user_id: &OwnedUserId,
    cache: &ReactionTracker,
) -> Option<InboundReaction> {
    let reaction = event
        .raw()
        .deserialize_as_unchecked::<OriginalSyncMessageLikeEvent<ReactionEventContent>>()
        .ok()?;
    if reaction.sender == *bot_user_id {
        return None;
    }
    let target_event_id = reaction.content.relates_to.event_id.clone();
    let key = cap_reaction_key(&reaction.content.relates_to.key);
    let conv = Owner::new(format!("{CHANNEL_NAME}:{room_id}"));
    let parent = MessageRef::top_level(CHANNEL_NAME, conv.clone(), target_event_id.to_string());
    let timestamp = matrix_timestamp_to_utc(Some(reaction.origin_server_ts));
    cache.record(
        reaction.event_id.clone(),
        TrackedReaction {
            target_event_id,
            key: key.clone(),
            sender: reaction.sender.clone(),
            room_id: room_id.clone(),
        },
    );
    Some(InboundReaction {
        channel: CHANNEL_NAME.into(),
        conv,
        from: Participant::new(
            ParticipantId::new(reaction.sender.to_string()),
            ParticipantRole::Human,
        ),
        parent,
        emoji: key,
        added: true,
        timestamp,
    })
}

/// Map a Matrix `m.room.redaction` timeline event to an inbound
/// reaction with `added: false` when the redacted event was a
/// previously tracked reaction. Returns `None` for redactions of
/// non-reaction events or for reactions outside the tracker window.
pub(crate) fn timeline_redaction_to_inbound_reaction(
    event: &TimelineEvent,
    bot_user_id: &OwnedUserId,
    cache: &ReactionTracker,
) -> Option<InboundReaction> {
    let redaction = event
        .raw()
        .deserialize_as_unchecked::<RawRedaction>()
        .ok()?;
    if redaction.event_type.as_deref() != Some("m.room.redaction") {
        return None;
    }
    if redaction.sender == *bot_user_id {
        return None;
    }
    let redacts = redaction
        .redacts
        .or_else(|| redaction.content.unwrap_or_default().redacts)?;
    let entry = cache.take(&redacts)?;
    let conv = Owner::new(format!("{CHANNEL_NAME}:{}", entry.room_id));
    let parent = MessageRef::top_level(
        CHANNEL_NAME,
        conv.clone(),
        entry.target_event_id.to_string(),
    );
    let timestamp = matrix_timestamp_to_utc(Some(redaction.origin_server_ts));
    // `from` reports the original reactor, matching the Slack
    // `reaction_removed` shape. Matrix admin-redactions of another
    // user's reaction therefore still surface as that reactor
    // retracting; consumer hooks consume the InboundReaction as a
    // semantic "alice no longer reacts" signal, not as an audit of
    // the redaction's actor.
    Some(InboundReaction {
        channel: CHANNEL_NAME.into(),
        conv,
        from: Participant::new(
            ParticipantId::new(entry.sender.to_string()),
            ParticipantRole::Human,
        ),
        parent,
        emoji: entry.key,
        added: false,
        timestamp,
    })
}

/// Minimal redaction shape: the `redacts` field lives at the top level
/// for room versions <= 10 and inside `content` for v11+. We tolerate
/// both by reading whichever path the event uses.
#[derive(Deserialize)]
struct RawRedaction {
    sender: OwnedUserId,
    origin_server_ts: MilliSecondsSinceUnixEpoch,
    #[serde(default, rename = "type")]
    event_type: Option<String>,
    #[serde(default)]
    redacts: Option<OwnedEventId>,
    #[serde(default)]
    content: Option<RawRedactionContent>,
}

#[derive(Default, Deserialize)]
struct RawRedactionContent {
    #[serde(default)]
    redacts: Option<OwnedEventId>,
}

#[cfg(test)]
mod test_helpers;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_sanitize;
