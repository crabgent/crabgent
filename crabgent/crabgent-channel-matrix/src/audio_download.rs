//! Matrix media download helpers for inbound audio support.

use bytes::Bytes;
use crabgent_channel::{CappedMediaBody, MAX_AUDIO_BYTES};
use crabgent_log::instrument;
use matrix_sdk::{Client, ruma::OwnedMxcUri};

/// Errors that can occur when downloading Matrix audio.
pub use crabgent_channel::MediaDownloadError as AudioDownloadError;

/// Download audio from Matrix media content by MXC URI.
#[instrument(level = "debug", skip(http_client, matrix_client, access_token))]
pub async fn download_matrix_audio(
    http_client: &reqwest::Client,
    matrix_client: &Client,
    source: &OwnedMxcUri,
    access_token: Option<&str>,
) -> Result<(Bytes, String), AudioDownloadError> {
    let server_name = source
        .server_name()
        .map_err(|_err| AudioDownloadError::Network)?;
    let media_id = source
        .media_id()
        .map_err(|_err| AudioDownloadError::Network)?;
    // Conduit and other modern Matrix homeservers refuse the legacy
    // unauthenticated `/_matrix/media/v3/download/...` path. Hit the
    // authenticated v1 client media endpoint instead, like
    // `download_matrix_image` already does.
    let download_url = matrix_client
        .homeserver()
        .join(&format!(
            "_matrix/client/v1/media/download/{server_name}/{media_id}"
        ))
        .map_err(|_err| AudioDownloadError::Network)?;

    let token = access_token
        .map(ToOwned::to_owned)
        .or_else(|| matrix_client.access_token())
        .ok_or(AudioDownloadError::Auth)?;

    let response = http_client
        .get(download_url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_err| AudioDownloadError::Network)?;

    let status = response.status();
    if status == 401 || status == 403 {
        return Err(AudioDownloadError::Auth);
    }
    if !status.is_success() {
        return Err(AudioDownloadError::Network);
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();

    if !content_type.starts_with("audio/") {
        return Err(AudioDownloadError::Mime);
    }

    let bytes = read_capped_audio_body(response).await?;

    Ok((bytes, content_type))
}

async fn read_capped_audio_body(
    mut response: reqwest::Response,
) -> Result<Bytes, AudioDownloadError> {
    let mut body = CappedMediaBody::new(MAX_AUDIO_BYTES, response.content_length())
        .map_err(|_err| AudioDownloadError::Size)?;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_err| AudioDownloadError::Network)?
    {
        body.push_chunk(&chunk)
            .map_err(|_err| AudioDownloadError::Size)?;
    }
    Ok(body.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::{Method::GET, MockServer};
    use url::Url;

    #[tokio::test]
    async fn download_matrix_audio_auth_failure_no_token_leak() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/_matrix/client/v1/media/download/localhost/audio-id")
                .header("authorization", "Bearer secret-test-audio-token-99999");
            then.status(401);
        });

        let client = Client::new(Url::parse(&server.base_url()).expect("mock server URL"))
            .await
            .expect("matrix client");
        let source: OwnedMxcUri = "mxc://localhost/audio-id".to_owned().into();
        let result = download_matrix_audio(
            &reqwest::Client::new(),
            &client,
            &source,
            Some("secret-test-audio-token-99999"),
        )
        .await;

        let error = result.expect_err("auth failure");
        assert!(matches!(error, AudioDownloadError::Auth));
        assert_eq!(format!("{error}"), "authentication failed");
        assert!(!format!("{error}").contains("secret-test-audio-token-99999"));
    }

    #[tokio::test]
    async fn rejects_oversize_audio_while_reading_body() {
        let server = MockServer::start();
        let body = vec![0_u8; usize::try_from(MAX_AUDIO_BYTES + 1).expect("audio cap fits usize")];
        server.mock(|when, then| {
            when.method(GET)
                .path("/_matrix/client/v1/media/download/localhost/audio-id")
                .header("authorization", "Bearer test-token");
            then.status(200)
                .header("content-type", "audio/ogg")
                .body(body);
        });

        let client = Client::new(Url::parse(&server.base_url()).expect("mock server URL"))
            .await
            .expect("matrix client");
        let source: OwnedMxcUri = "mxc://localhost/audio-id".to_owned().into();
        let error = download_matrix_audio(
            &reqwest::Client::new(),
            &client,
            &source,
            Some("test-token"),
        )
        .await
        .expect_err("oversize body");

        assert!(matches!(error, AudioDownloadError::Size));
    }
}
