mod common;

use crabgent_channel::{Channel, MessageRef};
use crabgent_channel_slack::SlackChannel;
use crabgent_core::{owner::Owner, subject::Subject};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

use common::{slack_client, slack_test_ctx};

#[tokio::test]
async fn channel_read_uses_conversations_history_without_thread_parent() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/conversations.history"))
        .and(body_string_contains("channel=C123"))
        .and(body_string_contains("limit=20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "messages": [{"ts": "1700000000.123456", "text": "top", "user": "U1"}]
        })))
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let conv = Owner::new("slack:T123/C123");

    let messages = channel
        .read(&Subject::new("agent"), &conv, None, 20)
        .await
        .expect("history");

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message_ref.id, "1700000000.123456");
    assert_eq!(messages[0].author.as_str(), "U1");
    assert_eq!(messages[0].body, "top");
    assert_eq!(messages[0].timestamp_unix_ms, 1_700_000_000_123);
    assert!(messages[0].message_ref.thread_root.is_none());
}

#[tokio::test]
async fn channel_read_uses_conversations_replies_with_thread_parent() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/conversations.replies"))
        .and(body_string_contains("channel=C123"))
        .and(body_string_contains("ts=1700000000.000100"))
        .and(body_string_contains("limit=5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "messages": [{"ts": "1700000001.000200", "text": "reply", "bot_id": "B1"}]
        })))
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let conv = Owner::new("slack:T123/C123");
    let parent = MessageRef::top_level("slack", conv.clone(), "1700000000.000100");

    let messages = channel
        .read(&Subject::new("agent"), &conv, Some(&parent), 5)
        .await
        .expect("replies");

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message_ref.id, "1700000001.000200");
    assert_eq!(
        messages[0].message_ref.thread_root(),
        Some("1700000000.000100")
    );
    assert_eq!(messages[0].author.as_str(), "B1");
    assert_eq!(messages[0].body, "reply");
}
