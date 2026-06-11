//! Slack audio attachment mapping.

use bytes::Bytes;
use crabgent_channel::{AudioValidator, CappedMediaBody, MAX_AUDIO_BYTES};
use crabgent_core::message::{AudioPayload, ContentBlock};
use reqwest::StatusCode;

use crate::events::SlackFileMetadata;

/// Download Slack audio bytes, validate them, and build a provider-neutral block.
pub async fn build_audio_attachment(
    client: &reqwest::Client,
    token: &str,
    audio_validator: &AudioValidator,
    file_metadata: &SlackFileMetadata,
    declared_mime: &str,
) -> ContentBlock {
    if file_metadata.size.unwrap_or(0) > audio_limit_i64() {
        return rejected("too large");
    }

    // Prefer the original-bytes URL: Slack transcodes audio uploads to MP4
    // for inline preview at `url_private`, which fails byte-level validation.
    let Some(url) = file_metadata
        .url_private_download
        .as_deref()
        .or(file_metadata.url_private.as_deref())
    else {
        return rejected("missing private URL");
    };

    let bytes = match download_audio(client, token, url).await {
        Ok(bytes) => bytes,
        Err(reason) => return rejected(reason),
    };

    match audio_validator.validate(&bytes, declared_mime) {
        Ok(()) => match AudioPayload::new(
            bytes.to_vec(),
            declared_mime.to_owned(),
            Some(file_metadata.id.clone()),
        ) {
            Ok(payload) => ContentBlock::Audio(payload),
            Err(error) => {
                crabgent_log::debug!(%error, "slack audio payload validation failed");
                rejected(error)
            }
        },
        Err(rejection) => rejected(rejection),
    }
}

async fn download_audio(
    client: &reqwest::Client,
    token: &str,
    url: &str,
) -> Result<Bytes, &'static str> {
    let response = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| {
            crabgent_log::debug!(%error, "slack audio download failed");
            "download failed"
        })?;

    ensure_audio_response_success(response.status())?;
    read_audio_body(response).await
}

fn ensure_audio_response_success(status: StatusCode) -> Result<(), &'static str> {
    if status.is_success() {
        return Ok(());
    }
    reject_audio_status(status)
}

fn reject_audio_status(status: StatusCode) -> Result<(), &'static str> {
    if is_audio_auth_status(status) {
        crabgent_log::warn!("slack audio download authentication failed");
        return Err("download failed");
    }
    crabgent_log::debug!(%status, "slack audio download returned non-success status");
    Err("download failed")
}

const fn is_audio_auth_status(status: StatusCode) -> bool {
    matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
}

async fn read_audio_body(mut response: reqwest::Response) -> Result<Bytes, &'static str> {
    let mut body = CappedMediaBody::new(MAX_AUDIO_BYTES, response.content_length())
        .map_err(|_err| "too large")?;
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        crabgent_log::debug!(%error, "slack audio body read failed");
        "download failed"
    })? {
        body.push_chunk(&chunk).map_err(|_err| "too large")?;
    }
    Ok(body.finish())
}

fn rejected(reason: impl std::fmt::Display) -> ContentBlock {
    ContentBlock::Text {
        text: format!("[Audio rejected: {reason}]"),
    }
}

fn audio_limit_i64() -> i64 {
    i64::try_from(MAX_AUDIO_BYTES).unwrap_or(0)
}
