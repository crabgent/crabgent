use crabgent_channel::AudioValidator;
use crabgent_channel::assemble_audio_attachment;
use crabgent_core::ContentBlock;
use crabgent_log::debug;
use secrecy::SecretString;

use crate::audio_download::{AudioDownloadError, download_telegram_audio_from_base};

use super::TelegramMessage;

pub(super) struct AudioAttachmentMeta<'a> {
    pub(super) file_id: &'a str,
    pub(super) declared_mime: &'a str,
    pub(super) duration: Option<i64>,
    pub(super) filename: Option<&'a str>,
}

pub(super) fn log_audio_attachment_skip(
    message: &TelegramMessage,
    audio_meta: &AudioAttachmentMeta<'_>,
    error: &AudioDownloadError,
) {
    debug!(
        %error,
        message_id = message.message_id,
        file_id = audio_meta.file_id,
        duration = ?audio_meta.duration,
        "telegram audio attachment skipped"
    );
}

pub(super) async fn build_telegram_audio_attachment(
    client: &reqwest::Client,
    api_base: &str,
    bot_token: &SecretString,
    validator: &AudioValidator,
    audio: &AudioAttachmentMeta<'_>,
) -> Result<ContentBlock, AudioDownloadError> {
    let (bytes, _downloaded_mime) =
        download_telegram_audio_from_base(client, api_base, bot_token, audio.file_id).await?;
    assemble_audio_attachment(
        &bytes,
        audio.declared_mime.to_owned(),
        audio.filename.map(str::to_owned),
        validator,
        "telegram audio",
    )
    .map_err(|_error| AudioDownloadError::Mime)
}
