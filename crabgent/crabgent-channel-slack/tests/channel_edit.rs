mod common;

use crabgent_channel::{Channel, ChannelError, MessageRef};
use crabgent_channel_slack::SlackChannel;
use crabgent_core::{owner::Owner, subject::Subject};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

use common::{slack_client, slack_test_ctx};

#[tokio::test]
async fn channel_edit_calls_chat_update() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/chat.update"))
        .and(body_json(serde_json::json!({
            "channel": "C123",
            "ts": "1.2",
            "text": "updated"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true, "channel": "C123", "ts": "1.2"
        })))
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let conv = Owner::new("slack:T123/C123");
    let target = MessageRef::top_level("slack", conv.clone(), "1.2");

    channel
        .edit(&Subject::new("agent"), &conv, &target, "updated")
        .await
        .expect("edit");
}

#[tokio::test]
async fn channel_edit_normalizes_markdown_before_chat_update() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/chat.update"))
        .and(body_json(serde_json::json!({
            "channel": "C123",
            "ts": "1.2",
            "text": "*updated* <https://docs.slack.dev|docs>"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true, "channel": "C123", "ts": "1.2"
        })))
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let conv = Owner::new("slack:T123/C123");
    let target = MessageRef::top_level("slack", conv.clone(), "1.2");

    channel
        .edit(
            &Subject::new("agent"),
            &conv,
            &target,
            "**updated** [docs](https://docs.slack.dev)",
        )
        .await
        .expect("edit");
}

#[tokio::test]
async fn channel_edit_maps_slack_api_error() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/chat.update"))
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
        .edit(&Subject::new("agent"), &conv, &target, "updated")
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
