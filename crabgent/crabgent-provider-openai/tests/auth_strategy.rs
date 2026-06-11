mod common;

use crabgent_provider_openai::{ApiKeyAuth, AuthStrategy, CodexOAuthAuth};
use reqwest::header::{AUTHORIZATION, HeaderName};
use secrecy::SecretString;

use crate::common::openai_test_ctx;

#[tokio::test]
async fn apikey_auth_sends_bearer_header() {
    let ctx = openai_test_ctx().await;
    let headers = ctx.api_key_auth.auth_headers();

    assert_header(&headers, "authorization", "Bearer secret-test-key-99999");
    assert_no_header(&headers, "originator");
}

#[tokio::test]
async fn codex_oauth_sends_bearer_and_codex_headers() {
    let ctx = openai_test_ctx().await;
    let headers = ctx.codex_auth.auth_headers();

    assert_header(&headers, "authorization", "Bearer secret-test-token-99999");
    assert_header(&headers, "openai-beta", "responses=experimental");
    assert_header(&headers, "originator", "codex_cli_rs");
    assert_header(&headers, "user-agent", "codex_cli_rs/0.59.0");
    assert_header(&headers, "chatgpt-account-id", "account-test-id");
}

#[tokio::test]
async fn apikey_endpoint_is_chat_completions() {
    let ctx = openai_test_ctx().await;
    let url = format!(
        "{}{}",
        ctx.api_key_auth.base_url(),
        ctx.api_key_auth.wire().endpoint_path()
    );

    assert_eq!(
        ctx.api_key_auth.wire().endpoint_path(),
        "/v1/chat/completions"
    );
    assert_eq!(url, format!("{}/v1/chat/completions", ctx.server.url()));
}

#[tokio::test]
async fn codex_endpoint_is_responses() {
    let ctx = openai_test_ctx().await;
    let url = format!(
        "{}{}",
        ctx.codex_auth.base_url(),
        ctx.codex_auth.wire().endpoint_path()
    );

    assert_eq!(
        ctx.codex_auth.wire().endpoint_path(),
        "/backend-api/codex/responses"
    );
    assert_eq!(
        url,
        format!("{}/backend-api/codex/responses", ctx.server.url())
    );
}

#[test]
fn apikey_debug_masks_key() {
    let auth = ApiKeyAuth::new(SecretString::from("secret-test-key-99999".to_owned()));
    let rendered = format!("{auth:?}");

    assert!(!rendered.contains("secret-test-key-99999"));
    assert!(rendered.contains("****<masked>"));
}

#[test]
fn codex_oauth_debug_masks_token() {
    let auth = CodexOAuthAuth::new(
        SecretString::from("secret-test-token-99999".to_owned()),
        Some("account-test-id".to_owned()),
    );
    let rendered = format!("{auth:?}");

    assert!(!rendered.contains("secret-test-token-99999"));
    assert!(rendered.contains("****<masked>"));
    assert!(rendered.contains("account-test-id"));
}

fn assert_header(
    headers: &[(HeaderName, reqwest::header::HeaderValue)],
    name: &str,
    expected: &str,
) {
    assert!(
        headers
            .iter()
            .any(|(header_name, value)| header_name.as_str() == name
                && value.to_str().ok() == Some(expected)),
        "missing header {name}={expected}"
    );
}

fn assert_no_header(headers: &[(HeaderName, reqwest::header::HeaderValue)], name: &str) {
    let forbidden = HeaderName::from_bytes(name.as_bytes()).expect("valid header name");
    assert!(
        headers
            .iter()
            .all(|(header_name, _)| header_name.as_str() != forbidden.as_str()),
        "unexpected header {name}"
    );
    assert!(
        headers
            .iter()
            .any(|(header_name, _)| header_name.as_str() == AUTHORIZATION.as_str()),
        "authorization header should still be present"
    );
}
