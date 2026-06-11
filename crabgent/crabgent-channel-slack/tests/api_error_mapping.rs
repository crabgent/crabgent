mod common;

use crabgent_channel_slack::SlackError;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use common::slack_test_ctx;

#[tokio::test]
async fn maps_slack_error_codes_and_http_status() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    Mock::given(method("POST"))
        .and(path("/chat.delete"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"ok": false, "error": "invalid_auth"})),
        )
        .mount(server)
        .await;

    let error = ctx
        .http_client_with_retry(2)
        .delete_message(&ctx.test_channel, "1.1")
        .await
        .expect_err("invalid_auth should map to auth");

    assert!(matches!(error, SlackError::Auth));
}

#[tokio::test]
async fn maps_non_success_http_status_after_json_decode() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    Mock::given(method("POST"))
        .and(path("/chat.delete"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"ok": true})))
        .mount(server)
        .await;

    let error = ctx
        .http_client_with_retry(2)
        .delete_message(&ctx.test_channel, "1.1")
        .await
        .expect_err("500 should map to API error");

    assert!(matches!(
        error,
        SlackError::ApiError {
            slack_code,
            http_status: Some(500)
        } if slack_code == "http_500"
    ));
}
