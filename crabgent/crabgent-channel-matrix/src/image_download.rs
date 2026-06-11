//! Matrix media download helpers for inbound image support.

use bytes::Bytes;
use crabgent_channel::{CappedMediaBody, MAX_IMAGE_BYTES};
use crabgent_log::instrument;
use matrix_sdk::{Client, ruma::OwnedMxcUri};

/// Errors that can occur when downloading a Matrix image.
pub use crabgent_channel::MediaDownloadError as ImageDownloadError;

/// Download an image from Matrix media content by MXC URI.
#[instrument(level = "debug", skip(http_client, matrix_client, access_token))]
pub async fn download_matrix_image(
    http_client: &reqwest::Client,
    matrix_client: &Client,
    source: &OwnedMxcUri,
    access_token: Option<&str>,
) -> Result<(Bytes, String), ImageDownloadError> {
    let server_name = source
        .server_name()
        .map_err(|_err| ImageDownloadError::Network)?;
    let media_id = source
        .media_id()
        .map_err(|_err| ImageDownloadError::Network)?;
    let download_url = matrix_client
        .homeserver()
        .join(&format!(
            "_matrix/client/v1/media/download/{server_name}/{media_id}"
        ))
        .map_err(|_err| ImageDownloadError::Network)?;

    let token = access_token
        .map(ToOwned::to_owned)
        .or_else(|| matrix_client.access_token())
        .ok_or(ImageDownloadError::Auth)?;

    let response = http_client
        .get(download_url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_err| ImageDownloadError::Network)?;

    let status = response.status();
    if status == 401 || status == 403 {
        return Err(ImageDownloadError::Auth);
    }
    if !status.is_success() {
        return Err(ImageDownloadError::Network);
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();

    if !content_type.starts_with("image/") {
        return Err(ImageDownloadError::Mime);
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::{Method::GET, MockServer};
    use matrix_sdk::{
        SessionMeta, SessionTokens,
        authentication::matrix::MatrixSession,
        ruma::{OwnedDeviceId, owned_user_id},
    };
    use url::Url;

    async fn client_with_access_token(base_url: &str, access_token: &str) -> Client {
        let client = Client::new(Url::parse(base_url).expect("mock URL"))
            .await
            .expect("matrix client");
        let session = MatrixSession {
            meta: SessionMeta {
                user_id: owned_user_id!("@bot:localhost"),
                device_id: OwnedDeviceId::from("DEVICEID"),
            },
            tokens: SessionTokens {
                access_token: access_token.to_owned(),
                refresh_token: None,
            },
        };
        client
            .restore_session(session)
            .await
            .expect("restore session");
        client
    }

    #[tokio::test]
    async fn download_matrix_image_auth_failure_no_token_leak() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/_matrix/client/v1/media/download/localhost/photo-id")
                .header("authorization", "Bearer secret-test-token-12345");
            then.status(401);
        });

        let client = Client::new(Url::parse(&server.base_url()).expect("mock server URL"))
            .await
            .expect("matrix client");
        let source: OwnedMxcUri = "mxc://localhost/photo-id".to_owned().into();
        let result = download_matrix_image(
            &reqwest::Client::new(),
            &client,
            &source,
            Some("secret-test-token-12345"),
        )
        .await;

        let error = result.expect_err("auth failure");
        assert!(matches!(error, ImageDownloadError::Auth));
        assert_eq!(format!("{error}"), "authentication failed");
        assert!(!format!("{error}").contains("secret-test-token-12345"));
    }

    #[tokio::test]
    async fn returns_auth_without_token_before_request() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/_matrix/client/v1/media/download/localhost/photo-id");
            then.status(200)
                .header("content-type", "image/png")
                .body(b"\x89PNG\r\n\x1a\n");
        });
        let client = Client::new(Url::parse(&server.base_url()).expect("mock URL"))
            .await
            .expect("matrix client");
        let source: OwnedMxcUri = "mxc://localhost/photo-id".to_owned().into();
        let error = download_matrix_image(&reqwest::Client::new(), &client, &source, None)
            .await
            .expect_err("missing token");

        assert!(matches!(error, ImageDownloadError::Auth));
        mock.assert_calls(0);
    }

    #[tokio::test]
    async fn uses_client_access_token_when_argument_is_none() {
        let server = MockServer::start();
        let png_bytes = b"\x89PNG\r\n\x1a\n";
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/_matrix/client/v1/media/download/localhost/photo-id")
                .header("authorization", "Bearer fallback-token");
            then.status(200)
                .header("content-type", "image/png")
                .body(png_bytes);
        });
        let client = client_with_access_token(&server.base_url(), "fallback-token").await;
        let source: OwnedMxcUri = "mxc://localhost/photo-id".to_owned().into();
        let (bytes, content_type) =
            download_matrix_image(&reqwest::Client::new(), &client, &source, None)
                .await
                .expect("success via client access token");

        mock.assert();
        assert!(!bytes.is_empty());
        assert_eq!(content_type, "image/png");
    }

    #[tokio::test]
    async fn rejects_oversize_image_while_reading_body() {
        let server = MockServer::start();
        let body = vec![0_u8; usize::try_from(MAX_IMAGE_BYTES + 1).expect("image cap fits usize")];
        server.mock(|when, then| {
            when.method(GET)
                .path("/_matrix/client/v1/media/download/localhost/photo-id")
                .header("authorization", "Bearer test-token");
            then.status(200)
                .header("content-type", "image/png")
                .body(body);
        });
        let client = Client::new(Url::parse(&server.base_url()).expect("mock URL"))
            .await
            .expect("matrix client");
        let source: OwnedMxcUri = "mxc://localhost/photo-id".to_owned().into();
        let error = download_matrix_image(
            &reqwest::Client::new(),
            &client,
            &source,
            Some("test-token"),
        )
        .await
        .expect_err("oversize body");

        assert!(matches!(error, ImageDownloadError::Size));
    }

    #[tokio::test]
    async fn maps_v1_path() {
        let server = MockServer::start();
        let png_bytes = b"\x89PNG\r\n\x1a\n";
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/_matrix/client/v1/media/download/localhost/photo-id")
                .header("authorization", "Bearer test-token");
            then.status(200)
                .header("content-type", "image/png")
                .body(png_bytes);
        });
        let client = Client::new(Url::parse(&server.base_url()).expect("mock URL"))
            .await
            .expect("matrix client");
        let source: OwnedMxcUri = "mxc://localhost/photo-id".to_owned().into();
        let (bytes, content_type) = download_matrix_image(
            &reqwest::Client::new(),
            &client,
            &source,
            Some("test-token"),
        )
        .await
        .expect("success");
        mock.assert();
        assert!(!bytes.is_empty());
        assert_eq!(content_type, "image/png");
    }
}
