//! Telegram audio download helpers.

use bytes::Bytes;
use crabgent_channel::{CappedMediaBody, MAX_AUDIO_BYTES};
use crabgent_log::instrument;
use secrecy::{ExposeSecret, SecretString};

/// Errors that can occur when downloading audio from Telegram.
pub use crabgent_channel::MediaDownloadError as AudioDownloadError;

#[instrument(level = "debug", skip(client, bot_token), fields(file_id = %file_id))]
pub async fn download_telegram_audio_from_base(
    client: &reqwest::Client,
    api_base: &str,
    bot_token: &SecretString,
    file_id: &str,
) -> Result<(Bytes, String), AudioDownloadError> {
    let bot_token = bot_token.expose_secret();
    let get_file_url = format!("{api_base}/bot{bot_token}/getFile");
    let response = client
        .post(&get_file_url)
        .json(&serde_json::json!({ "file_id": file_id }))
        .send()
        .await
        .map_err(|_err| AudioDownloadError::Network)?;

    if !response.status().is_success() {
        return if matches!(response.status().as_u16(), 401 | 403) {
            Err(AudioDownloadError::Auth)
        } else {
            Err(AudioDownloadError::Network)
        };
    }

    let value = response
        .json::<serde_json::Value>()
        .await
        .map_err(|_err| AudioDownloadError::Network)?;
    let file_path = value
        .get("result")
        .and_then(|result| result.get("file_path"))
        .and_then(serde_json::Value::as_str)
        .ok_or(AudioDownloadError::Network)?;

    let file_path_url = format!("{api_base}/file/bot{bot_token}/{file_path}");
    let response = client
        .get(&file_path_url)
        .send()
        .await
        .map_err(|_err| AudioDownloadError::Network)?;

    match response.status().as_u16() {
        401 | 403 => return Err(AudioDownloadError::Auth),
        200..=299 => {}
        _ => return Err(AudioDownloadError::Network),
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
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
    use httpmock::Method::{GET, POST};
    use httpmock::MockServer;
    use serde_json::json;

    const TEST_BOT_TOKEN: &str = "secret-test-token-audio-12345";

    fn test_bot_token() -> SecretString {
        SecretString::from(TEST_BOT_TOKEN.to_owned())
    }

    #[tokio::test]
    async fn telegram_audio_download_auth_failure_no_token_leak() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST)
                .path("/botsecret-test-token-audio-12345/getFile");
            then.status(403);
        });

        let client = reqwest::Client::new();
        let result = download_telegram_audio_from_base(
            &client,
            &server.base_url(),
            &test_bot_token(),
            "audio-id",
        )
        .await;

        let err = result.expect_err("authentication failed");
        assert!(matches!(err, AudioDownloadError::Auth));
        let err_text = format!("{err}");
        assert_eq!(err_text, "authentication failed");
        assert!(!err_text.contains(TEST_BOT_TOKEN));
    }

    #[tokio::test]
    async fn telegram_audio_rejects_oversize_content_length() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST)
                .path("/botsecret-test-token-audio-12345/getFile");
            then.status(200).json_body(json!({
                "ok": true,
                "result": {"file_path": "audio/voice-id.ogg"},
            }));
        });
        server.mock(|when, then| {
            let oversize =
                vec![0_u8; usize::try_from(MAX_AUDIO_BYTES + 1).expect("audio cap fits usize")];
            when.method(GET)
                .path("/file/botsecret-test-token-audio-12345/audio/voice-id.ogg");
            then.status(200)
                .header("content-type", "audio/ogg")
                .body(oversize);
        });

        let client = reqwest::Client::new();
        let err = download_telegram_audio_from_base(
            &client,
            &server.base_url(),
            &test_bot_token(),
            "voice-id",
        )
        .await
        .expect_err("oversize content-length");

        assert!(matches!(err, AudioDownloadError::Size));
    }
}
