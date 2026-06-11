//! Long-polling Telegram inbound driver.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, TimeZone, Utc};
use crabgent_channel::{
    AudioValidator, ChannelError, ChannelInbox, ChannelKind, IMAGE_PROCESSING_FALLBACK_BODY,
    ImageStore, ImageValidator, InboundBody, InboundEvent, InboundEventBuilder, InboundParticipant,
    ParticipantRole,
};
use crabgent_core::ContentBlock;
use crabgent_core::owner::Owner;
use crabgent_log::{debug, error, instrument, warn};
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::json;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::{
    channel::TelegramChannel,
    photo_types::{PhotoSize, select_best_photo_size},
};

mod audio_attachment;
mod commands;
mod image_attachment;
pub(crate) mod reaction;
pub mod test_helpers;
use audio_attachment::{
    AudioAttachmentMeta, build_telegram_audio_attachment, log_audio_attachment_skip,
};
use image_attachment::build_telegram_image_attachment;
pub use test_helpers::build_update_json;

const DEFAULT_POLL_TIMEOUT_SECS: u64 = 25;
const DEFAULT_BACKOFF_SECS: u64 = 5;
const TELEGRAM_PRIVATE_TYPE: &str = "private";

/// Long-polling driver wrapping a [`TelegramChannel`].
pub struct TelegramPoller {
    channel: Arc<TelegramChannel>,
    inbox: Arc<dyn ChannelInbox>,
    commands: Option<commands::CommandConfig>,
    poll_timeout: Duration,
    error_backoff: Duration,
    last_offset: Option<i64>,
    bot_token: SecretString,
    image_client: reqwest::Client,
    audio_http_client: reqwest::Client,
    image_store: Option<Arc<dyn ImageStore>>,
    image_validator: Option<ImageValidator>,
    audio_validator: Option<AudioValidator>,
}

impl TelegramPoller {
    /// Build a poller over `channel`, dispatching events to `inbox`.
    pub fn new(channel: Arc<TelegramChannel>, inbox: Arc<dyn ChannelInbox>) -> Self {
        let bot_token = channel.bot_token().clone();
        Self {
            channel,
            inbox,
            commands: None,
            poll_timeout: Duration::from_secs(DEFAULT_POLL_TIMEOUT_SECS),
            error_backoff: Duration::from_secs(DEFAULT_BACKOFF_SECS),
            last_offset: None,
            bot_token,
            image_client: crate::http::build_media_client()
                .expect("hardened media client has no fallible configuration"),
            audio_http_client: crate::http::build_media_client()
                .expect("hardened media client has no fallible configuration"),
            image_store: None,
            image_validator: None,
            audio_validator: None,
        }
    }

    /// Enable image attachments in inbound mapping.
    #[must_use]
    pub fn with_image_support(
        mut self,
        image_client: reqwest::Client,
        image_store: Arc<dyn ImageStore>,
        image_validator: ImageValidator,
    ) -> Self {
        self.image_client = image_client;
        self.image_store = Some(image_store);
        self.image_validator = Some(image_validator);
        self
    }

    /// Enable audio attachments in inbound mapping.
    #[must_use]
    pub fn with_audio_support(
        mut self,
        audio_http_client: reqwest::Client,
        audio_validator: AudioValidator,
    ) -> Self {
        self.audio_http_client = audio_http_client;
        self.audio_validator = Some(audio_validator);
        self
    }

    /// Override the long-poll `timeout` parameter (default 25 s).
    #[must_use]
    pub const fn with_poll_timeout(mut self, d: Duration) -> Self {
        self.poll_timeout = d;
        self
    }

    /// Override the back-off used after an error (default 5 s).
    #[must_use]
    pub const fn with_error_backoff(mut self, d: Duration) -> Self {
        self.error_backoff = d;
        self
    }

    /// Run the polling loop until `cancel` fires.
    #[instrument(level = "debug", skip(self, cancel))]
    pub async fn run(mut self, cancel: CancellationToken) -> Result<(), ChannelError> {
        loop {
            if cancel.is_cancelled() {
                debug!("telegram poller cancelled");
                return Ok(());
            }
            tokio::select! {
                () = cancel.cancelled() => return Ok(()),
                res = self.tick_once() => {
                    if let Err(err) = res {
                        warn!("telegram poller tick failed: {err}");
                        tokio::select! {
                            () = cancel.cancelled() => return Ok(()),
                            () = sleep(self.error_backoff) => {}
                        }
                    }
                }
            }
        }
    }

    /// Run a single `getUpdates` round and dispatch its updates.
    ///
    /// Exposed for deterministic integration tests; production code
    /// drives the poller via [`Self::run`].
    #[doc(hidden)]
    pub async fn tick_once(&mut self) -> Result<(), ChannelError> {
        let updates = fetch_updates_for(&self.channel, self.poll_timeout, self.last_offset).await?;
        let inbox = self.dispatch_inbox();
        for update in updates {
            if update.update_id <= self.last_offset.unwrap_or(i64::MIN) {
                continue;
            }
            let update_id = update.update_id;
            if let Some(event) = self.update_to_event(&update).await
                && let Err(err) = inbox.receive(event).await
            {
                error!(update_id, error = %err, "telegram inbox receive failed");
                return Err(err);
            }
            if let Some(mr) = update.message_reaction.as_ref() {
                reaction::dispatch_reactions(&inbox, update_id, mr).await?;
            }
            self.advance_offset(update_id);
        }
        Ok(())
    }

    async fn update_to_event(&self, update: &TelegramUpdate) -> Option<InboundEvent> {
        let message = private_message(update)?;
        let text = message_text(message);
        let from = message.from.as_ref()?;
        let attachments = self.message_attachments(message).await;
        let body = event_body(
            text,
            &attachments,
            message_has_photo(message),
            self.image_support_enabled(),
            self.image_store.is_some(),
        )?;
        Self::build_event_or_warn(message, from, body, attachments)
    }

    async fn message_attachments(&self, message: &TelegramMessage) -> Vec<ContentBlock> {
        let mut attachments = self.image_attachments(message).await;
        self.push_audio_attachment(message, &mut attachments).await;
        attachments
    }

    async fn image_attachments(&self, message: &TelegramMessage) -> Vec<ContentBlock> {
        let selected_photo = message
            .photo
            .as_ref()
            .and_then(|photos| select_best_photo_size(photos));
        if let (Some(store), Some(validator), Some(photo)) =
            (&self.image_store, &self.image_validator, selected_photo)
        {
            vec![
                build_telegram_image_attachment(
                    &self.image_client,
                    self.channel.api_base(),
                    &self.bot_token,
                    store.as_ref(),
                    validator,
                    photo,
                )
                .await,
            ]
        } else {
            Vec::new()
        }
    }

    async fn push_audio_attachment(
        &self,
        message: &TelegramMessage,
        attachments: &mut Vec<ContentBlock>,
    ) {
        if let (Some(validator), Some(audio_meta)) =
            (&self.audio_validator, message.audio_attachment_meta())
        {
            self.push_audio_meta(message, attachments, validator, &audio_meta)
                .await;
        }
    }

    async fn push_audio_meta(
        &self,
        message: &TelegramMessage,
        attachments: &mut Vec<ContentBlock>,
        validator: &AudioValidator,
        audio_meta: &AudioAttachmentMeta<'_>,
    ) {
        match build_telegram_audio_attachment(
            &self.audio_http_client,
            self.channel.api_base(),
            &self.bot_token,
            validator,
            audio_meta,
        )
        .await
        {
            Ok(block) => attachments.push(block),
            Err(error) => log_audio_attachment_skip(message, audio_meta, &error),
        }
    }

    const fn image_support_enabled(&self) -> bool {
        self.image_store.is_some() && self.image_validator.is_some()
    }

    fn build_event_or_warn(
        message: &TelegramMessage,
        from: &TelegramUser,
        body: &str,
        attachments: Vec<ContentBlock>,
    ) -> Option<InboundEvent> {
        match Self::build_event(message, from, body, attachments) {
            Ok(event) => Some(event),
            Err(err) => {
                warn!(%err, "dropping oversized telegram inbound text");
                None
            }
        }
    }

    fn build_event(
        message: &TelegramMessage,
        from: &TelegramUser,
        body: &str,
        attachments: Vec<ContentBlock>,
    ) -> Result<InboundEvent, ChannelError> {
        let chat_id = message.chat.id;
        let conv = Owner::new(format!("telegram:{chat_id}"));
        let body = InboundBody::new(body)?;
        let mut participant = InboundParticipant::new(from.id.to_string(), ParticipantRole::Human);
        if let Some(name) = display_name(from) {
            participant = participant.with_display_name(name);
        }
        let mut builder = InboundEventBuilder::new(
            "telegram",
            conv,
            message.message_id.to_string(),
            participant,
            body,
            timestamp_to_utc(message.date),
        )
        .kind(ChannelKind::Direct)
        .attachments(attachments);
        if let Some(thread_id) = message.message_thread_id {
            builder = builder.thread_root(thread_id.to_string());
        }
        Ok(builder.build())
    }

    fn advance_offset(&mut self, update_id: i64) {
        if update_id > self.last_offset.unwrap_or(i64::MIN) {
            self.last_offset = Some(update_id);
        }
    }
}

fn private_message(update: &TelegramUpdate) -> Option<&TelegramMessage> {
    let message = update.message.as_ref()?;
    if message.chat.chat_type != TELEGRAM_PRIVATE_TYPE {
        return None;
    }
    Some(message)
}

fn message_text(message: &TelegramMessage) -> &str {
    message
        .caption
        .as_deref()
        .or(message.text.as_deref())
        .unwrap_or("")
}

const fn message_has_photo(message: &TelegramMessage) -> bool {
    message.photo.is_some()
}

const fn event_body<'a>(
    text: &'a str,
    attachments: &[ContentBlock],
    has_photo: bool,
    image_support_enabled: bool,
    image_store_enabled: bool,
) -> Option<&'a str> {
    let body = selected_event_body(text, attachments, has_photo, image_support_enabled);
    if body.is_empty() && should_drop_empty_body(attachments, has_photo, image_store_enabled) {
        return None;
    }
    Some(body)
}

const fn selected_event_body<'a>(
    text: &'a str,
    attachments: &[ContentBlock],
    has_photo: bool,
    image_support_enabled: bool,
) -> &'a str {
    if text.is_empty() && attachments.is_empty() && has_photo && image_support_enabled {
        return IMAGE_PROCESSING_FALLBACK_BODY;
    }
    text
}

const fn should_drop_empty_body(
    attachments: &[ContentBlock],
    has_photo: bool,
    image_store_enabled: bool,
) -> bool {
    (!image_store_enabled && has_photo) || attachments.is_empty()
}

async fn fetch_updates_for(
    channel: &TelegramChannel,
    poll_timeout: Duration,
    last_offset: Option<i64>,
) -> Result<Vec<TelegramUpdate>, ChannelError> {
    // Voice and audio are message sub-types and do not need separate allowed_updates entries.
    // `message_reaction` is opt-in: Telegram only delivers it when explicitly listed here.
    let body = json!({
        "timeout": poll_timeout.as_secs(),
        "offset": last_offset.map(|o| o + 1),
        "allowed_updates": ["message", "message_reaction"],
    });
    let value = channel.post_json("getUpdates", &body).await?;
    let result = value
        .get("result")
        .cloned()
        .ok_or_else(|| ChannelError::adapter("getUpdates missing result"))?;
    serde_json::from_value(result).map_err(ChannelError::Serde)
}

fn display_name(from: &TelegramUser) -> Option<String> {
    from.username.clone().or_else(|| {
        let first = from.first_name.as_deref().unwrap_or("");
        let last = from.last_name.as_deref().unwrap_or("");
        let combined = format!("{first} {last}").trim().to_owned();
        if combined.is_empty() {
            None
        } else {
            Some(combined)
        }
    })
}

fn timestamp_to_utc(secs: i64) -> DateTime<Utc> {
    if let Some(dt) = Utc.timestamp_opt(secs, 0).single() {
        dt
    } else {
        warn!(timestamp_raw = secs, "invalid update timestamp; using now");
        Utc::now()
    }
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    #[serde(default)]
    message: Option<TelegramMessage>,
    #[serde(default)]
    message_reaction: Option<reaction::TelegramMessageReactionUpdated>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    date: i64,
    chat: TelegramChat,
    #[serde(default)]
    from: Option<TelegramUser>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    caption: Option<String>,
    #[serde(default)]
    photo: Option<Vec<PhotoSize>>,
    #[serde(default)]
    voice: Option<TelegramVoice>,
    #[serde(default)]
    audio: Option<TelegramAudio>,
    #[serde(default)]
    message_thread_id: Option<i64>,
}

impl TelegramMessage {
    fn audio_attachment_meta(&self) -> Option<AudioAttachmentMeta<'_>> {
        if let Some(voice) = &self.voice {
            return Some(AudioAttachmentMeta {
                file_id: &voice.file_id,
                declared_mime: voice.mime_type.as_deref().unwrap_or("audio/ogg"),
                duration: voice.duration,
                filename: None,
            });
        }

        self.audio.as_ref().map(|audio| AudioAttachmentMeta {
            file_id: &audio.file_id,
            declared_mime: audio.mime_type.as_deref().unwrap_or("audio/mpeg"),
            duration: audio.duration,
            filename: audio.file_name.as_deref(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct TelegramVoice {
    file_id: String,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    duration: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TelegramAudio {
    file_id: String,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    duration: Option<i64>,
    #[serde(default)]
    file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TelegramChat {
    pub(crate) id: i64,
    #[serde(rename = "type")]
    pub(crate) chat_type: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TelegramUser {
    pub(crate) id: i64,
    #[serde(default)]
    pub(crate) username: Option<String>,
    #[serde(default)]
    pub(crate) first_name: Option<String>,
    #[serde(default)]
    pub(crate) last_name: Option<String>,
}

#[cfg(test)]
mod command_tests;
#[cfg(test)]
mod recording_inbox;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_audio;
#[cfg(test)]
mod tests_sanitize;
