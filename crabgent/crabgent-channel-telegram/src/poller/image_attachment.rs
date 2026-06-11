use crabgent_channel::{
    ImageStore, ImageValidator, assemble_image_attachment, image_download_size_fallback,
    image_processing_fallback,
};
use crabgent_core::ContentBlock;
use crabgent_log::debug;
use secrecy::SecretString;

use crate::image_download::{ImageDownloadError, download_telegram_photo_from_base};
use crate::photo_types::PhotoSize;

pub(super) async fn build_telegram_image_attachment(
    client: &reqwest::Client,
    api_base: &str,
    bot_token: &SecretString,
    store: &dyn ImageStore,
    validator: &ImageValidator,
    photo: &PhotoSize,
) -> ContentBlock {
    let (bytes, declared_mime) = match download_telegram_photo_from_base(
        client,
        api_base,
        bot_token,
        &photo.file_id,
    )
    .await
    {
        Ok(download) => download,
        Err(error) => {
            debug!(
                %error,
                file_id = photo.file_id,
                "telegram image download failed"
            );
            return image_download_fallback(&error);
        }
    };
    assemble_image_attachment(bytes, &declared_mime, store, validator, "telegram image").await
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
