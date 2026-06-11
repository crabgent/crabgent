//! Telegram image download helpers.

use bytes::Bytes;
use crabgent_channel::{CappedMediaBody, MAX_IMAGE_BYTES};
use crabgent_log::instrument;
use secrecy::{ExposeSecret, SecretString};

/// Errors that can occur when downloading an image from Telegram.
pub use crabgent_channel::MediaDownloadError as ImageDownloadError;

/// Download an image by Telegram `file_id`.
///
/// The bot token is used only to build Telegram Bot API file URLs.
/// It never appears in tracing output or error text.
#[instrument(level = "debug", skip(client, bot_token), fields(file_id = %file_id))]
pub async fn download_telegram_photo(
    client: &reqwest::Client,
    bot_token: &SecretString,
    file_id: &str,
) -> Result<(Bytes, String), ImageDownloadError> {
    download_telegram_photo_from_base(client, "https://api.telegram.org", bot_token, file_id).await
}

#[instrument(level = "debug", skip(client, bot_token), fields(file_id = %file_id))]
pub(crate) async fn download_telegram_photo_from_base(
    client: &reqwest::Client,
    api_base: &str,
    bot_token: &SecretString,
    file_id: &str,
) -> Result<(Bytes, String), ImageDownloadError> {
    let bot_token = bot_token.expose_secret();
    let get_file_url = format!("{api_base}/bot{bot_token}/getFile");
    let response = client
        .post(&get_file_url)
        .json(&serde_json::json!({ "file_id": file_id }))
        .send()
        .await
        .map_err(|_err| ImageDownloadError::Network)?;

    if !response.status().is_success() {
        return if response.status().as_u16() == 401 || response.status().as_u16() == 403 {
            Err(ImageDownloadError::Auth)
        } else {
            Err(ImageDownloadError::Network)
        };
    }

    let value = response
        .json::<serde_json::Value>()
        .await
        .map_err(|_err| ImageDownloadError::Network)?;

    let file_path = value
        .get("result")
        .and_then(|result| result.get("file_path"))
        .and_then(serde_json::Value::as_str)
        .ok_or(ImageDownloadError::Network)?;

    let file_path_url = format!("{api_base}/file/bot{bot_token}/{file_path}");
    let response = client
        .get(&file_path_url)
        .send()
        .await
        .map_err(|_err| ImageDownloadError::Network)?;

    match response.status().as_u16() {
        401 | 403 => return Err(ImageDownloadError::Auth),
        200..=299 => {}
        _ => return Err(ImageDownloadError::Network),
    }

    let header_content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();

    // Telegram's file CDN frequently serves images as `application/octet-stream`
    // (no proper Content-Type). Fall back to extension-based MIME inference so
    // the consumer-side `ImageValidator` (magic-byte + whitelist) can still gate
    // the bytes correctly. If neither header nor extension yields a known
    // image MIME, reject.
    let content_type = if header_content_type.starts_with("image/") {
        header_content_type
    } else if let Some(mime) = mime_from_path_extension(file_path) {
        mime.to_owned()
    } else {
        return Err(ImageDownloadError::Mime);
    };

    let bytes = read_capped_image_body(response).await?;

    Ok((bytes, content_type))
}

async fn read_capped_image_body(
    mut response: reqwest::Response,
) -> Result<Bytes, ImageDownloadError> {
    let mut body = CappedMediaBody::new(MAX_IMAGE_BYTES, response.content_length())
        .map_err(|_err| ImageDownloadError::Size)?;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_err| ImageDownloadError::Network)?
    {
        body.push_chunk(&chunk)
            .map_err(|_err| ImageDownloadError::Size)?;
    }
    Ok(body.finish())
}

/// Map a Telegram `file_path` extension to its canonical image MIME type.
/// Only the four MIME types that `crabgent_channel::ALLOWED_MIMES` accepts
/// are mapped here; anything else returns `None` so the caller falls back
/// to `ImageDownloadError::Mime`.
fn mime_from_path_extension(file_path: &str) -> Option<&'static str> {
    let ext = file_path.rsplit('.').next()?.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::{GET, POST};
    use httpmock::MockServer;
    use serde_json::json;

    const TEST_BOT_TOKEN: &str = "secret-test-token-12345";

    fn test_bot_token() -> SecretString {
        SecretString::from(TEST_BOT_TOKEN.to_owned())
    }

    fn minimal_png_bytes() -> Vec<u8> {
        let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(
            b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00",
        );
        png
    }

    #[tokio::test]
    async fn telegram_image_download_happy_path() {
        let server = MockServer::start();
        let png = minimal_png_bytes();

        server.mock(|when, then| {
            when.method(POST)
                .path("/botsecret-test-token-12345/getFile");
            then.status(200).json_body(json!({
                "ok": true,
                "result": {"file_path": "photos/photo-id.png"},
            }));
        });

        server.mock(|when, then| {
            when.method(GET)
                .path("/file/botsecret-test-token-12345/photos/photo-id.png");
            then.status(200)
                .header("content-type", "image/png")
                .body(png.clone());
        });

        let client = reqwest::Client::new();
        let result = download_telegram_photo_from_base(
            &client,
            &server.base_url(),
            &test_bot_token(),
            "photo-id",
        )
        .await;

        let (bytes, mime) = result.expect("download ok");
        assert_eq!(bytes.to_vec(), png);
        assert_eq!(mime, "image/png");
    }

    #[tokio::test]
    async fn telegram_image_download_auth_failure_no_token_leak() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST)
                .path("/botsecret-test-token-12345/getFile");
            then.status(401);
        });

        let client = reqwest::Client::new();
        let result = download_telegram_photo_from_base(
            &client,
            &server.base_url(),
            &test_bot_token(),
            "photo-id",
        )
        .await;
        let err = result.expect_err("authentication failed");
        assert!(matches!(err, ImageDownloadError::Auth));
        let err_text = format!("{err}");
        assert_eq!(err_text, "authentication failed");
        assert!(!err_text.contains(TEST_BOT_TOKEN));
    }

    #[tokio::test]
    async fn telegram_image_size_limit_graceful_fallback() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(POST)
                .path("/botsecret-test-token-12345/getFile");
            then.status(200).json_body(json!({
                "ok": true,
                "result": {"file_path": "photos/large.png"},
            }));
        });

        let big = vec![0u8; 6_000_000];
        server.mock(|when, then| {
            when.method(GET)
                .path("/file/botsecret-test-token-12345/photos/large.png");
            then.status(200)
                .header("content-type", "image/png")
                .body(big);
        });

        let client = reqwest::Client::new();
        let result = download_telegram_photo_from_base(
            &client,
            &server.base_url(),
            &test_bot_token(),
            "photo-id",
        )
        .await;
        assert!(matches!(result, Err(ImageDownloadError::Size)));
    }

    #[tokio::test]
    async fn telegram_image_octet_stream_falls_back_to_extension() {
        // Telegram's file CDN sometimes serves images as
        // `application/octet-stream` instead of `image/<sub>`. The download
        // function must fall back to extension-based MIME so the consumer-side
        // ImageValidator can verify by magic bytes.
        let server = MockServer::start();
        let png = minimal_png_bytes();

        server.mock(|when, then| {
            when.method(POST)
                .path("/botsecret-test-token-12345/getFile");
            then.status(200).json_body(json!({
                "ok": true,
                "result": {"file_path": "photos/file_42.png"},
            }));
        });

        server.mock(|when, then| {
            when.method(GET)
                .path("/file/botsecret-test-token-12345/photos/file_42.png");
            then.status(200)
                .header("content-type", "application/octet-stream")
                .body(png.clone());
        });

        let client = reqwest::Client::new();
        let (bytes, mime) = download_telegram_photo_from_base(
            &client,
            &server.base_url(),
            &test_bot_token(),
            "photo-id",
        )
        .await
        .expect("octet-stream image download with .png extension should succeed");
        assert_eq!(bytes.to_vec(), png);
        assert_eq!(mime, "image/png");
    }

    #[tokio::test]
    async fn telegram_image_octet_stream_unknown_extension_rejected() {
        // Octet-stream + unknown extension must still fail. We don't want to
        // start treating arbitrary binaries as images.
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(POST)
                .path("/botsecret-test-token-12345/getFile");
            then.status(200).json_body(json!({
                "ok": true,
                "result": {"file_path": "documents/file_42.pdf"},
            }));
        });

        server.mock(|when, then| {
            when.method(GET)
                .path("/file/botsecret-test-token-12345/documents/file_42.pdf");
            then.status(200)
                .header("content-type", "application/octet-stream")
                .body(vec![0u8; 16]);
        });

        let client = reqwest::Client::new();
        let result = download_telegram_photo_from_base(
            &client,
            &server.base_url(),
            &test_bot_token(),
            "photo-id",
        )
        .await;
        assert!(matches!(result, Err(ImageDownloadError::Mime)));
    }

    #[test]
    fn mime_from_path_extension_known_image_extensions() {
        assert_eq!(mime_from_path_extension("photos/x.jpg"), Some("image/jpeg"));
        assert_eq!(
            mime_from_path_extension("photos/X.JPEG"),
            Some("image/jpeg")
        );
        assert_eq!(mime_from_path_extension("photos/x.png"), Some("image/png"));
        assert_eq!(mime_from_path_extension("photos/x.gif"), Some("image/gif"));
        assert_eq!(
            mime_from_path_extension("photos/x.webp"),
            Some("image/webp")
        );
        assert_eq!(mime_from_path_extension("photos/x.pdf"), None);
        assert_eq!(mime_from_path_extension("photos/x"), None);
    }
}
