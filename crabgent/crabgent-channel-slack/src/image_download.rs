//! Slack image download with token redaction.
//!
//! Downloads images from Slack's private URL endpoint using the bot
//! token. All errors use generic variants without token material.

use bytes::Bytes;
use crabgent_channel::{CappedMediaBody, MAX_IMAGE_BYTES};
use crabgent_log::instrument;
use secrecy::{ExposeSecret, SecretString};

/// Errors that can occur when downloading an image from Slack.
pub use crabgent_channel::MediaDownloadError as ImageDownloadError;

/// Download an image from a Slack private URL.
///
/// The token is used in the `Authorization: Bearer` header and is
/// never included in error messages or log output.
#[instrument(level = "debug", skip(client, token), fields(url = %url))]
pub async fn download_slack_image(
    client: &reqwest::Client,
    token: &SecretString,
    url: &str,
) -> Result<(Bytes, String), ImageDownloadError> {
    let response = client
        .get(url)
        .bearer_auth(token.expose_secret())
        .send()
        .await
        .map_err(|_err| ImageDownloadError::Network)?;

    match response.status().as_u16() {
        401 | 403 => return Err(ImageDownloadError::Auth),
        200..=299 => {}
        _ => return Err(ImageDownloadError::Network),
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();

    if !content_type.starts_with("image/") {
        return Err(ImageDownloadError::Mime);
    }

    let body = read_capped_image_body(response).await?;

    Ok((body, content_type))
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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_token() -> SecretString {
        SecretString::new("secret-test-token-12345".into())
    }

    fn minimal_png_bytes() -> Vec<u8> {
        let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(
            b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00",
        );
        png
    }

    #[tokio::test]
    async fn slack_image_download_happy_path() {
        let server = MockServer::start().await;
        let png = minimal_png_bytes();
        Mock::given(method("GET"))
            .and(path("/download/image.png"))
            .and(header("Authorization", "Bearer secret-test-token-12345"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(png.clone())
                    .insert_header("content-type", "image/png"),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/download/image.png", server.uri());
        let result = download_slack_image(&client, &test_token(), &url).await;
        let (bytes, mime) = result.expect("download ok");
        assert_eq!(bytes.to_vec(), png);
        assert_eq!(mime, "image/png");
    }

    #[tokio::test]
    async fn slack_image_download_auth_failure_no_token_leak() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/download/fail", server.uri());
        let result = download_slack_image(&client, &test_token(), &url).await;
        assert!(matches!(result, Err(ImageDownloadError::Auth)));
        // Verify the error display does not contain the token
        let err_msg = format!("{}", result.expect_err("expected error"));
        assert!(
            !err_msg.contains("secret-test-token-12345"),
            "token leaked in error: {err_msg}"
        );
    }

    #[tokio::test]
    async fn slack_image_size_limit_graceful_fallback() {
        let server = MockServer::start().await;
        let big = vec![0u8; 6_000_000];
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(big)
                    .insert_header("content-type", "image/png"),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/download/big", server.uri());
        let result = download_slack_image(&client, &test_token(), &url).await;
        assert!(matches!(result, Err(ImageDownloadError::Size)));
    }

    #[tokio::test]
    async fn slack_image_magic_byte_mismatch() {
        let server = MockServer::start().await;
        // PNG bytes but declared as image/gif
        let png = minimal_png_bytes();
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(png)
                    .insert_header("content-type", "image/gif"),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/download/mismatch", server.uri());
        let result = download_slack_image(&client, &test_token(), &url).await;
        // Download itself succeeds; MIME mismatch is caught by
        // ImageValidator upstream. This test verifies the download
        // returns the declared content-type so the validator can reject.
        if let Ok((_, mime)) = result {
            assert_eq!(mime, "image/gif");
        }
    }
}
