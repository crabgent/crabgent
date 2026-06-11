//! Slack event to `crabgent-channel` inbound mapping.

pub mod audio;
mod slack_ts;

pub use slack_ts::parse_slack_ts;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use crabgent_channel::{
    AudioValidator, ChannelKind, ImageStore, ImageValidator, InboundBody, InboundEvent,
    InboundEventBuilder, InboundParticipant, InboundReaction, MessageRef, Participant,
    ParticipantRole, assemble_image_attachment, image_download_size_fallback,
    image_processing_fallback,
};
use crabgent_core::message::ContentBlock;
use crabgent_core::owner::Owner;
use secrecy::{ExposeSecret, SecretString};

use self::audio::build_audio_attachment;
use crate::CHANNEL_NAME;
use crate::events::{SlackAssistantThreadEvent, SlackEvent, SlackFileMetadata, SlackMessageEvent};
use crate::ids::{SlackChannelId, SlackOwner, SlackWorkspaceId};
use crate::image_download::{ImageDownloadError, download_slack_image};

pub type ChannelKindCache = Arc<Mutex<HashMap<String, ChannelKind>>>;
pub type ChannelTypeCache = Arc<Mutex<HashMap<String, String>>>;

struct AttachmentServices<'a> {
    client: &'a reqwest::Client,
    token: &'a SecretString,
    store: &'a dyn ImageStore,
    image_validator: &'a ImageValidator,
    audio_validator: &'a AudioValidator,
}

#[must_use]
pub fn new_channel_kind_cache() -> ChannelKindCache {
    Arc::new(Mutex::new(HashMap::new()))
}

#[must_use]
pub fn new_channel_type_cache() -> ChannelTypeCache {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Convert a Slack event into an inbound channel event.
///
/// This is the canonical inbound conversion entry point so callers keep the
/// channel-type cache and validators alive across events. The former short
/// wrapper allocated a fresh `AudioValidator` per call and only had test
/// consumers, so the lean path is to call this dependency-explicit function.
#[expect(
    clippy::too_many_arguments,
    reason = "adapter conversion needs the Slack event plus distinct runtime dependencies"
)]
pub async fn slack_event_to_inbound_with_channel_type_cache(
    event: &SlackEvent,
    workspace_id: &SlackWorkspaceId,
    kind_cache: &ChannelKindCache,
    type_cache: &ChannelTypeCache,
    self_bot_id: Option<&str>,
    client: &reqwest::Client,
    token: &SecretString,
    store: &dyn ImageStore,
    validator: &ImageValidator,
    audio_validator: &AudioValidator,
) -> Option<InboundEvent> {
    match event {
        SlackEvent::Message(message) => {
            let attachment_services = AttachmentServices {
                client,
                token,
                store,
                image_validator: validator,
                audio_validator,
            };
            message_to_inbound(
                message,
                workspace_id,
                kind_cache,
                type_cache,
                self_bot_id,
                &attachment_services,
            )
            .await
        }
        SlackEvent::AppMention(message) => {
            message_to_group_inbound(message, workspace_id, kind_cache, type_cache, self_bot_id)
        }
        SlackEvent::AssistantThreadStarted(thread)
        | SlackEvent::AssistantThreadContextChanged(thread) => {
            assistant_thread_to_inbound(thread, workspace_id, kind_cache)
        }
        SlackEvent::ReactionAdded(_)
        | SlackEvent::ReactionRemoved(_)
        | SlackEvent::MemberJoinedChannel(_)
        | SlackEvent::Other { .. } => None,
    }
}

/// Convert a Slack reaction event into an inbound reaction event.
///
/// Returns `None` for events the kernel cannot map: missing user
/// (anonymous reactions), bot self-reactions (echo loop with
/// `Channel::react`), or an unknown channel id. The channel kind falls
/// back to the kind cache, defaulting to `ChannelKind::Group` when the
/// channel has not been observed yet (mirrors the `file_shared`
/// precedent).
pub fn slack_event_to_inbound_reaction(
    event: &SlackEvent,
    workspace_id: &SlackWorkspaceId,
    kind_cache: &ChannelKindCache,
    self_user_id: Option<&str>,
) -> Option<InboundReaction> {
    let (reaction, added) = match event {
        SlackEvent::ReactionAdded(r) => (r, true),
        SlackEvent::ReactionRemoved(r) => (r, false),
        _ => return None,
    };
    reaction_to_inbound(reaction, workspace_id, kind_cache, self_user_id, added)
}

fn reaction_to_inbound(
    reaction: &crate::events::SlackReactionEvent,
    workspace_id: &SlackWorkspaceId,
    kind_cache: &ChannelKindCache,
    self_user_id: Option<&str>,
    added: bool,
) -> Option<InboundReaction> {
    let user = reaction.user.as_deref()?;
    if let Some(user_id) = self_user_id
        && user == user_id
    {
        return None;
    }
    let owner = slack_owner(workspace_id, &reaction.item.channel)?;
    // Touch the kind cache so the slack subject resolver picks the
    // channel kind without re-querying conversations.info. Missing
    // entries default to Group (file_shared precedent).
    let _ = kind_cache
        .lock()
        .expect("channel kind cache")
        .entry(reaction.item.channel.clone())
        .or_insert(ChannelKind::Group);
    let parent = MessageRef::top_level(CHANNEL_NAME, owner.clone(), &reaction.item.ts);
    // Derive the event instant from the reacted-to message's Slack ts so
    // startup-cutoff filtering treats replayed reactions on reconnect as past
    // events (mirrors message_to_inbound). Fall back to now only when the wire
    // ts is absent or unparseable.
    let timestamp = parse_slack_ts(&reaction.item.ts).unwrap_or_else(Utc::now);
    Some(InboundReaction {
        channel: CHANNEL_NAME.to_owned(),
        conv: owner,
        from: Participant::new(user, ParticipantRole::Human),
        parent,
        emoji: reaction.reaction.clone(),
        added,
        timestamp,
    })
}

async fn message_to_inbound(
    message: &SlackMessageEvent,
    workspace_id: &SlackWorkspaceId,
    kind_cache: &ChannelKindCache,
    type_cache: &ChannelTypeCache,
    self_bot_id: Option<&str>,
    attachment_services: &AttachmentServices<'_>,
) -> Option<InboundEvent> {
    if self_bot_id.is_some() && message.bot_id.as_deref() == self_bot_id {
        return None; // Own bot echo.
    }
    let user = message.user.as_deref().or(message.bot_id.as_deref())?;
    cache_type(
        &message.channel,
        message.channel_type.as_deref(),
        type_cache,
    );
    let kind = channel_kind(
        message.channel_type.as_deref(),
        &message.channel,
        kind_cache,
    );
    let is_bot = message.subtype.as_deref() == Some("bot_message");
    let role = if is_bot {
        ParticipantRole::Bot
    } else {
        ParticipantRole::Human
    };
    let attachments = file_share_attachments(message, attachment_services).await;
    build_message_event(
        workspace_id,
        &message.channel,
        user,
        message.text.as_deref().unwrap_or_default(),
        &message.ts,
        message.thread_ts.as_deref(),
        kind,
        role,
        attachments,
    )
}

async fn file_share_attachments(
    message: &SlackMessageEvent,
    services: &AttachmentServices<'_>,
) -> Vec<ContentBlock> {
    // Slack sends file uploads via message events with a non-empty `files`
    // array. `subtype="file_share"` is common but not guaranteed (e.g. for
    // app-driven `files.completeUploadExternal` with `initial_comment`).
    // Process whenever the message carries files.
    let Some(files) = message.files.as_deref().filter(|files| !files.is_empty()) else {
        return vec![];
    };

    let mut attachments = Vec::with_capacity(files.len());
    for file in files {
        add_file_attachment(file, services, &mut attachments).await;
    }
    attachments
}

async fn add_file_attachment(
    file: &SlackFileMetadata,
    services: &AttachmentServices<'_>,
    attachments: &mut Vec<ContentBlock>,
) {
    let Some(mime) = file.mimetype.as_deref() else {
        return;
    };

    if mime.starts_with("image/") {
        add_image_attachment(file, services, mime, attachments).await;
        return;
    }

    if mime.starts_with("audio/") {
        attachments.push(
            build_audio_attachment(
                services.client,
                services.token.expose_secret(),
                services.audio_validator,
                file,
                mime,
            )
            .await,
        );
    }
}

async fn add_image_attachment(
    file: &SlackFileMetadata,
    services: &AttachmentServices<'_>,
    mime: &str,
    attachments: &mut Vec<ContentBlock>,
) {
    let Some(url) = file.url_private.as_deref() else {
        return;
    };

    attachments.push(
        build_image_attachment(
            services.client,
            services.token,
            services.store,
            services.image_validator,
            url,
            mime,
        )
        .await,
    );
}

fn message_to_group_inbound(
    message: &SlackMessageEvent,
    workspace_id: &SlackWorkspaceId,
    kind_cache: &ChannelKindCache,
    type_cache: &ChannelTypeCache,
    self_bot_id: Option<&str>,
) -> Option<InboundEvent> {
    if self_bot_id.is_some() && message.bot_id.as_deref() == self_bot_id {
        return None;
    }
    cache_kind(&message.channel, ChannelKind::Group, kind_cache);
    cache_type(
        &message.channel,
        message.channel_type.as_deref(),
        type_cache,
    );
    let is_bot = message.subtype.as_deref() == Some("bot_message");
    let role = if is_bot {
        ParticipantRole::Bot
    } else {
        ParticipantRole::Human
    };
    build_message_event(
        workspace_id,
        &message.channel,
        message.user.as_deref().or(message.bot_id.as_deref())?,
        message.text.as_deref().unwrap_or_default(),
        &message.ts,
        message.thread_ts.as_deref(),
        ChannelKind::Group,
        role,
        vec![],
    )
}

/// Download an image from Slack, validate it, cache it, and build a
/// `ContentBlock::Image`.
async fn build_image_attachment(
    client: &reqwest::Client,
    token: &SecretString,
    store: &dyn ImageStore,
    validator: &ImageValidator,
    url: &str,
    declared_mime: &str,
) -> ContentBlock {
    let (bytes, _response_mime) = match download_slack_image(client, token, url).await {
        Ok(download) => download,
        Err(error) => {
            crabgent_log::debug!(%error, "slack image download failed");
            return image_download_fallback(&error);
        }
    };

    assemble_image_attachment(bytes, declared_mime, store, validator, "slack image").await
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

fn assistant_thread_to_inbound(
    thread: &SlackAssistantThreadEvent,
    workspace_id: &SlackWorkspaceId,
    cache: &ChannelKindCache,
) -> Option<InboundEvent> {
    cache_kind(&thread.channel, ChannelKind::Direct, cache);
    // Drop userless assistant-thread events instead of collapsing them onto a
    // shared `slack:unknown` subject (mirrors message_to_inbound, which returns
    // None when neither user nor bot id is present).
    let user = thread.user.as_deref()?;
    build_message_event(
        workspace_id,
        &thread.channel,
        user,
        "assistant_thread_started",
        &thread.thread_ts,
        Some(&thread.thread_ts),
        ChannelKind::Direct,
        ParticipantRole::Human,
        vec![],
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "message event construction maps distinct Slack wire fields without inventing a temporary type"
)]
fn build_message_event(
    workspace_id: &SlackWorkspaceId,
    channel: &str,
    user: &str,
    text: &str,
    ts: &str,
    thread_ts: Option<&str>,
    kind: ChannelKind,
    role: ParticipantRole,
    attachments: Vec<ContentBlock>,
) -> Option<InboundEvent> {
    let owner = slack_owner(workspace_id, channel)?;
    let timestamp = parse_slack_ts(ts).unwrap_or_else(Utc::now);
    let participant = InboundParticipant::new(user, role);
    let thread_root = thread_ts.or_else(|| (kind == ChannelKind::Direct).then_some(ts));
    let body = match InboundBody::new(text) {
        Ok(body) => body,
        Err(err) => {
            crabgent_log::warn!(%err, "dropping oversized inbound text");
            return None;
        }
    };
    let mut builder =
        InboundEventBuilder::new(CHANNEL_NAME, owner, ts, participant, body, timestamp)
            .kind(kind)
            .attachments(attachments);
    if let Some(root) = thread_root {
        builder = builder.thread_root(root);
    }
    Some(builder.build())
}

fn slack_owner(workspace_id: &SlackWorkspaceId, channel: &str) -> Option<Owner> {
    let channel_id = SlackChannelId::new(channel).ok()?;
    Some(SlackOwner::new(workspace_id.clone(), channel_id).owner())
}

fn channel_kind(
    channel_type: Option<&str>,
    channel: &str,
    cache: &ChannelKindCache,
) -> ChannelKind {
    let kind = match channel_type {
        Some("im") => ChannelKind::Direct,
        Some("mpim" | "channel" | "group") => ChannelKind::Group,
        _ => cache
            .lock()
            .expect("channel kind cache")
            .get(channel)
            .copied()
            .unwrap_or(ChannelKind::Group),
    };
    cache_kind(channel, kind, cache);
    kind
}

fn cache_kind(channel: &str, kind: ChannelKind, cache: &ChannelKindCache) {
    cache
        .lock()
        .expect("channel kind cache")
        .insert(channel.to_owned(), kind);
}

fn cache_type(channel: &str, channel_type: Option<&str>, cache: &ChannelTypeCache) {
    let Some(channel_type) = channel_type else {
        return;
    };
    cache
        .lock()
        .expect("channel type cache")
        .insert(channel.to_owned(), channel_type.to_owned());
}

#[cfg(test)]
mod tests;
