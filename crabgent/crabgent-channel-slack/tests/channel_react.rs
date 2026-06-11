mod common;

use crabgent_channel::{Channel, ChannelError, MessageRef};
use crabgent_channel_slack::SlackChannel;
use crabgent_core::{owner::Owner, subject::Subject};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

use common::{slack_client, slack_test_ctx};

#[tokio::test]
async fn channel_react_strips_colon_wrapped_emoji_name() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/reactions.add"))
        .and(body_string_contains("channel=C123"))
        .and(body_string_contains("timestamp=1.2"))
        .and(body_string_contains("name=eyes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let conv = Owner::new("slack:T123/C123");
    let parent = MessageRef::top_level("slack", conv.clone(), "1.2");

    let sent = channel
        .react(&Subject::new("agent"), &conv, &parent, ":eyes:")
        .await
        .expect("slack reaction should succeed");

    assert_eq!(sent.channel, "slack");
    assert_eq!(sent.conv, conv);
    assert_eq!(sent.id, "1.2");
    assert!(sent.thread_root.is_none());
}

#[tokio::test]
async fn channel_react_sends_bare_emoji_name_unchanged() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/reactions.add"))
        .and(body_string_contains("channel=C123"))
        .and(body_string_contains("timestamp=1.2"))
        .and(body_string_contains("name=thumbsup"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let conv = Owner::new("slack:T123/C123");
    let parent = MessageRef::top_level("slack", conv.clone(), "1.2");

    let sent = channel
        .react(&Subject::new("agent"), &conv, &parent, "thumbsup")
        .await
        .expect("slack reaction should succeed");

    assert_eq!(sent.id, "1.2");
}

#[tokio::test]
async fn channel_react_maps_slack_api_error_to_channel_error() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/reactions.add"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": false, "error": "already_reacted"})),
        )
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let conv = Owner::new("slack:T123/C123");
    let parent = MessageRef::top_level("slack", conv.clone(), "1.2");

    let err = channel
        .react(&Subject::new("agent"), &conv, &parent, "eyes")
        .await
        .expect_err("slack api error should map to channel error");

    assert!(matches!(err, ChannelError::Adapter(_)));
}
