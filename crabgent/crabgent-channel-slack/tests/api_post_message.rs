mod common;

use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, ResponseTemplate};

use common::slack_test_ctx;

#[tokio::test]
async fn post_message_sends_thread_ts() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .and(header("authorization", "Bearer bot-test-token"))
        .and(body_partial_json(json!({
            "channel": ctx.test_channel,
            "text": "hello thread",
            "thread_ts": "1710000000.000100",
            "mrkdwn": true
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "channel": "C123", "ts": "1710000000.000200"})),
        )
        .expect(1)
        .mount(server)
        .await;

    let response = ctx
        .http_client_with_retry(2)
        .post_message(
            &ctx.test_channel,
            "hello thread",
            Some("1710000000.000100"),
            false,
            true,
        )
        .await
        .expect("post should succeed");

    assert_eq!(response.channel.as_deref(), Some("C123"));
    assert_eq!(response.ts.as_deref(), Some("1710000000.000200"));
}
