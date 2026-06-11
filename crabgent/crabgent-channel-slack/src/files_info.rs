//! Slack `files.info` lookup support.

#[cfg(any(test, feature = "test-support"))]
use std::future::Future;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use thiserror::Error;

use crate::events::SlackFileMetadata;

const SLACK_API_BASE: &str = "https://slack.com/api";

#[cfg(any(test, feature = "test-support"))]
tokio::task_local! {
    static FILES_INFO_API_BASE: String;
}

/// Run a future with a temporary `files.info` API base URL.
#[doc(hidden)]
#[cfg(any(test, feature = "test-support"))]
pub async fn with_files_info_api_base<T>(api_base: String, future: impl Future<Output = T>) -> T {
    FILES_INFO_API_BASE.scope(api_base, future).await
}

/// Errors returned by the Slack `files.info` lookup.
#[derive(Debug, Error)]
pub enum FilesInfoError {
    /// Transport failure while calling Slack.
    #[error("network error")]
    Network(#[source] reqwest::Error),
    /// Slack returned an API or HTTP error.
    #[error("Slack API error")]
    Slack(String),
    /// Slack returned a malformed response body.
    #[error("decode error")]
    Decode(String),
}

/// Fetch metadata for a Slack file id via `files.info`.
#[crabgent_log::instrument(level = "debug", skip(client, token), fields(file_id = %file_id))]
pub async fn fetch_file_info(
    client: &reqwest::Client,
    token: &SecretString,
    file_id: &str,
) -> Result<SlackFileMetadata, FilesInfoError> {
    let response = client
        .get(files_info_url())
        .bearer_auth(token.expose_secret())
        .query(&[("file", file_id)])
        .send()
        .await
        .map_err(FilesInfoError::Network)?;

    let status = response.status();
    if !status.is_success() {
        return Err(FilesInfoError::Slack(format!("http_{}", status.as_u16())));
    }

    let body = response.text().await.map_err(FilesInfoError::Network)?;
    let payload: FilesInfoResponse =
        serde_json::from_str(&body).map_err(|error| FilesInfoError::Decode(error.to_string()))?;

    if !payload.ok {
        return Err(FilesInfoError::Slack(
            payload.error.unwrap_or_else(|| "unknown_error".to_owned()),
        ));
    }

    payload
        .file
        .ok_or_else(|| FilesInfoError::Decode("missing file".to_owned()))
}

#[cfg(any(test, feature = "test-support"))]
fn files_info_url() -> String {
    let api_base = FILES_INFO_API_BASE
        .try_with(Clone::clone)
        .unwrap_or_else(|_| SLACK_API_BASE.to_owned());
    format!("{}/files.info", api_base.trim_end_matches('/'))
}

#[cfg(not(any(test, feature = "test-support")))]
fn files_info_url() -> String {
    format!("{SLACK_API_BASE}/files.info")
}

#[derive(Deserialize)]
struct FilesInfoResponse {
    ok: bool,
    #[serde(default)]
    file: Option<SlackFileMetadata>,
    #[serde(default)]
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TEST_TOKEN: &str = "secret-test-token-99999";

    #[tokio::test]
    async fn files_info_happy_path() {
        let server = MockServer::start().await;
        mount_files_info(
            &server,
            "F123",
            ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "file": {
                    "id": "F123",
                    "mimetype": "audio/mpeg",
                    "url_private": "https://files.slack.com/F123",
                    "size": 42
                }
            })),
        )
        .await;

        let client = reqwest::Client::new();
        let token = SecretString::new(TEST_TOKEN.into());
        let metadata = with_files_info_api_base(api_base(&server), async {
            fetch_file_info(&client, &token, "F123").await
        })
        .await
        .expect("files.info metadata");

        assert_eq!(metadata.id, "F123");
        assert_eq!(metadata.mimetype.as_deref(), Some("audio/mpeg"));
        assert_eq!(
            metadata.url_private.as_deref(),
            Some("https://files.slack.com/F123")
        );
        assert_eq!(metadata.size, Some(42));
    }

    #[tokio::test]
    async fn files_info_ok_false_returns_slack_error() {
        let server = MockServer::start().await;
        mount_files_info(
            &server,
            "F404",
            ResponseTemplate::new(200).set_body_json(json!({
                "ok": false,
                "error": "file_not_found"
            })),
        )
        .await;

        let client = reqwest::Client::new();
        let token = SecretString::new(TEST_TOKEN.into());
        let result = with_files_info_api_base(api_base(&server), async {
            fetch_file_info(&client, &token, "F404").await
        })
        .await;

        let Err(FilesInfoError::Slack(code)) = result else {
            panic!("expected Slack error, got {result:?}");
        };
        assert_eq!(code, "file_not_found");
    }

    #[tokio::test]
    async fn files_info_http_401_has_no_token_in_display() {
        let server = MockServer::start().await;
        mount_files_info(
            &server,
            "F401",
            ResponseTemplate::new(401).set_body_string(TEST_TOKEN),
        )
        .await;

        let client = reqwest::Client::new();
        let token = SecretString::new(TEST_TOKEN.into());
        let result = with_files_info_api_base(api_base(&server), async {
            fetch_file_info(&client, &token, "F401").await
        })
        .await;

        let Err(error) = result else {
            panic!("expected auth failure");
        };
        assert!(matches!(error, FilesInfoError::Slack(_)));
        assert!(
            !error.to_string().contains(TEST_TOKEN),
            "files.info error leaked token: {error}"
        );
    }

    async fn mount_files_info(server: &MockServer, file_id: &str, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path("/api/files.info"))
            .and(query_param("file", file_id))
            .and(header("Authorization", format!("Bearer {TEST_TOKEN}")))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    fn api_base(server: &MockServer) -> String {
        format!("{}/api", server.uri())
    }
}
