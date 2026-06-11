//! HTTP helpers for Slack Web API calls.

use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Client, Method, Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::SlackError;

/// Build a reqwest client for Slack API calls.
pub fn build_client(timeout: Duration) -> Result<Client, SlackError> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(SlackError::Transport)
}

/// Build a reqwest client for Slack media downloads (`url_private`).
///
/// Media fetches carry the bot token, so the client must not follow
/// redirects (SSRF: a malicious payload could redirect to an internal
/// host and leak the bearer credential). Downloads are byte-capped and
/// finite, so a total request `timeout` is the correct `DoS` guard.
pub fn build_media_client(timeout: Duration) -> Result<Client, SlackError> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(SlackError::Transport)
}

/// Build the Slack bearer authorization header.
pub fn auth_headers(token: &SecretString) -> Result<HeaderMap, SlackError> {
    let mut headers = HeaderMap::new();
    let value = format!("Bearer {}", token.expose_secret());
    let value = HeaderValue::from_str(&value).map_err(|_err| SlackError::InvalidToken)?;
    headers.insert(AUTHORIZATION, value);
    Ok(headers)
}

/// Send a JSON Slack Web API request.
pub async fn send_json<T: DeserializeOwned + Send, B: Serialize + Sync>(
    client: &Client,
    token: &SecretString,
    url: &str,
    body: &B,
) -> Result<T, SlackError> {
    let mut headers = auth_headers(token)?;
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let response = client
        .request(Method::POST, url)
        .headers(headers)
        .json(body)
        .send()
        .await?;
    decode_slack_response(response).await
}

/// Send an `application/x-www-form-urlencoded` Slack Web API request.
pub async fn send_form<T: DeserializeOwned + Send, B: Serialize + Sync>(
    client: &Client,
    token: &SecretString,
    url: &str,
    body: &B,
) -> Result<T, SlackError> {
    let mut headers = auth_headers(token)?;
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    let response = client
        .request(Method::POST, url)
        .headers(headers)
        .form(body)
        .send()
        .await?;
    decode_slack_response(response).await
}

/// Decode Slack's common `{ ok: bool, ... }` response shape.
pub async fn decode_slack_response<T: DeserializeOwned>(
    response: Response,
) -> Result<T, SlackError> {
    let status = response.status();
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(SlackError::RateLimited {
            retry_after: retry_after(response.headers()),
        });
    }
    let text = response.text().await?;
    let value: Value = serde_json::from_str(&text)?;
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        let code = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown_error");
        return Err(SlackError::from_slack_code(
            code.to_owned(),
            Some(status.as_u16()),
        ));
    }
    if !status.is_success() {
        return Err(SlackError::ApiError {
            slack_code: format!("http_{}", status.as_u16()),
            http_status: Some(status.as_u16()),
        });
    }
    serde_json::from_value(value).map_err(SlackError::Serde)
}

/// Extract `Retry-After` seconds.
#[must_use]
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn media_client_does_not_follow_redirect() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/private"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/leak", server.uri())),
            )
            .expect(1)
            .mount(&server)
            .await;
        // A redirect-following client would re-attach the bearer token to
        // this target. With `Policy::none()` it must never be reached.
        Mock::given(method("GET"))
            .and(path("/leak"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let client = build_media_client(Duration::from_secs(5)).expect("media client should build");
        let response = client
            .get(format!("{}/private", server.uri()))
            .bearer_auth("secret-test-token-12345")
            .send()
            .await
            .expect("request should complete");

        // The 302 is surfaced verbatim instead of being followed.
        assert_eq!(response.status().as_u16(), 302);
        // `.expect(0)` on the redirect target is verified on `server` drop.
    }
}
