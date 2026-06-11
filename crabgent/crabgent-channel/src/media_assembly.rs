//! Adapter-neutral media attachment assembly.

use bytes::Bytes;
use crabgent_core::{AudioPayload, ContentBlock, ImagePayload};
use thiserror::Error;

use crate::audio_validation::{AudioRejection, AudioValidator};
use crate::image_store::ImageStore;
use crate::image_validation::{ImageValidator, MAX_IMAGE_BYTES, image_rejection_fallback_text};

pub const IMAGE_PROCESSING_FALLBACK_BODY: &str = "[image could not be processed]";

pub async fn assemble_image_attachment(
    bytes: Bytes,
    declared_mime: &str,
    store: &dyn ImageStore,
    validator: &ImageValidator,
    context: &'static str,
) -> ContentBlock {
    let validated_mime = match validate_image_attachment(&bytes, declared_mime, validator, context)
    {
        Ok(mime) => mime,
        Err(fallback) => return *fallback,
    };

    if let Err(error) = store.put(bytes.clone(), validated_mime).await {
        crabgent_log::debug!(%error, context, "image store put failed");
        return image_processing_fallback();
    }

    image_payload_block(&bytes, validated_mime, context)
}

fn validate_image_attachment(
    bytes: &Bytes,
    declared_mime: &str,
    validator: &ImageValidator,
    context: &'static str,
) -> Result<&'static str, Box<ContentBlock>> {
    validator
        .validate(bytes, declared_mime)
        .map_err(|rejection| {
            crabgent_log::debug!(%rejection, context, "image validation failed");
            Box::new(ContentBlock::Text {
                text: image_rejection_fallback_text(&rejection),
            })
        })
}

fn image_payload_block(bytes: &Bytes, validated_mime: &str, context: &'static str) -> ContentBlock {
    match ImagePayload::new(bytes.to_vec(), validated_mime.to_owned()) {
        Ok(payload) => ContentBlock::Image(payload),
        Err(error) => {
            crabgent_log::debug!(%error, context, "image payload validation failed");
            image_processing_fallback()
        }
    }
}

pub fn assemble_audio_attachment(
    bytes: &Bytes,
    declared_mime: String,
    filename: Option<String>,
    validator: &AudioValidator,
    context: &'static str,
) -> Result<ContentBlock, AudioAssemblyError> {
    validator
        .validate(bytes, &declared_mime)
        .map_err(|rejection| {
            crabgent_log::debug!(%rejection, context, "audio validation failed");
            AudioAssemblyError::Rejected(rejection)
        })?;

    let payload = AudioPayload::new(bytes.to_vec(), declared_mime, filename).map_err(|error| {
        crabgent_log::debug!(%error, context, "audio payload validation failed");
        AudioAssemblyError::Payload
    })?;
    Ok(ContentBlock::Audio(payload))
}

pub fn image_download_size_fallback() -> ContentBlock {
    ContentBlock::Text {
        text: format!("[image too large: max {MAX_IMAGE_BYTES} bytes]"),
    }
}

pub fn image_processing_fallback() -> ContentBlock {
    ContentBlock::Text {
        text: IMAGE_PROCESSING_FALLBACK_BODY.to_owned(),
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AudioAssemblyError {
    #[error("audio rejected: {0}")]
    Rejected(AudioRejection),
    #[error("audio payload could not be assembled")]
    Payload,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_size_fallback_uses_channel_cap() {
        let ContentBlock::Text { text } = image_download_size_fallback() else {
            panic!("expected text fallback");
        };
        assert_eq!(text, "[image too large: max 5000000 bytes]");
    }

    #[test]
    fn image_processing_fallback_is_stable() {
        let ContentBlock::Text { text } = image_processing_fallback() else {
            panic!("expected text fallback");
        };
        assert_eq!(text, IMAGE_PROCESSING_FALLBACK_BODY);
    }
}
