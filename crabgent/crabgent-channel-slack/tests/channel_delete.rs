mod common;

use crabgent_channel::{Channel, ChannelError, MessageRef};
use crabgent_channel_slack::SlackChannel;
use crabgent_core::{owner::Owner, subject::Subject};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

use common::{slack_client, slack_test_ctx};

#[tokio::test]
async fn channel_delete_calls_chat_delete() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/chat.delete"))
        .and(body_string_contains("channel=C123"))
        .and(body_string_contains("ts=1.2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true, "channel": "C123", "ts": "1.2"
        })))
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let conv = Owner::new("slack:T123/C123");
    let target = MessageRef::top_level("slack", conv.clone(), "1.2");

    channel
        .delete(&Subject::new("agent"), &conv, &target)
        .await
        .expect("delete");
}

#[tokio::test]
async fn channel_delete_maps_slack_api_error() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/chat.delete"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": false, "error": "message_not_found"})),
        )
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let conv = Owner::new("slack:T123/C123");
    let target = MessageRef::top_level("slack", conv.clone(), "1.2");

    let err = channel
        .delete(&Subject::new("agent"), &conv, &target)
        .await
        .expect_err("api error");

    // `ChannelError::Adapter` Display is opaque for LLM-safety; the
    // underlying Slack-API code is preserved in the inner field for
    // Debug + tracing, so destructure and assert against the field.
    let ChannelError::Adapter(detail) = &err else {
        panic!("expected Adapter, got {err:?}");
    };
    assert!(detail.contains("message_not_found"), "{detail}");
}
