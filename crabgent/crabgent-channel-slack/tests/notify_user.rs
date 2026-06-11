mod common;

use crabgent_channel::{Channel, ChannelError, OutboundMessage, ParticipantId};
use crabgent_channel_slack::{SlackChannel, SlackWorkspaceId};
use crabgent_core::subject::Subject;
use serde_json::json;
use wiremock::matchers::{body_partial_json, body_string_contains, header, method, path};
use wiremock::{Mock, ResponseTemplate};

use common::slack_test_ctx;

fn workspace() -> SlackWorkspaceId {
    SlackWorkspaceId::new("T123").expect("test workspace id should validate")
}

#[tokio::test]
async fn notify_user_opens_dm_then_posts_message() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/conversations.open"))
        .and(header("authorization", "Bearer bot-test-token"))
        .and(body_string_contains("users=U999"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channel": {"id": "D456"}
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .and(header("authorization", "Bearer bot-test-token"))
        .and(body_partial_json(json!({
            "channel": "D456",
            "text": "ping"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channel": "D456",
            "ts": "1710000000.000200"
        })))
        .mount(server)
        .await;

    let channel = SlackChannel::new(common::slack_client(&ctx)).with_workspace_id(workspace());
    let recipient = ParticipantId::new("U999");
    let sent = channel
        .notify_user(
            &Subject::new("agent"),
            &recipient,
            &OutboundMessage::new("ping"),
        )
        .await
        .expect("notify_user should succeed");

    assert_eq!(sent.channel, "slack");
    assert_eq!(sent.conv.as_str(), "slack:T123/D456");
    assert_eq!(sent.id, "1710000000.000200");
    assert!(sent.thread_root.is_none());
}

#[tokio::test]
async fn notify_user_surfaces_user_not_found_as_adapter_error() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/conversations.open"))
        .and(body_string_contains("users=UNOPE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": false,
            "error": "user_not_found"
        })))
        .mount(server)
        .await;

    let channel = SlackChannel::new(common::slack_client(&ctx)).with_workspace_id(workspace());
    let err = channel
        .notify_user(
            &Subject::new("agent"),
            &ParticipantId::new("UNOPE"),
            &OutboundMessage::new("ping"),
        )
        .await
        .expect_err("user_not_found must surface");

    let ChannelError::Adapter(msg) = err else {
        panic!("expected adapter error, got {err:?}");
    };
    assert!(
        msg.contains("user_not_found"),
        "adapter error should retain the Slack error code: {msg}"
    );
}

#[tokio::test]
async fn notify_user_requires_workspace_id_set() {
    let ctx = slack_test_ctx().await;
    let channel = SlackChannel::new(common::slack_client(&ctx));
    let err = channel
        .notify_user(
            &Subject::new("agent"),
            &ParticipantId::new("U999"),
            &OutboundMessage::new("ping"),
        )
        .await
        .expect_err("must require workspace id");
    let ChannelError::Adapter(msg) = err else {
        panic!("expected adapter error, got {err:?}");
    };
    assert!(
        msg.contains("workspace_id"),
        "error must mention missing workspace_id: {msg}"
    );
}

#[tokio::test]
async fn notify_user_rejects_invalid_slack_user_id() {
    let ctx = slack_test_ctx().await;
    let channel = SlackChannel::new(common::slack_client(&ctx)).with_workspace_id(workspace());
    let err = channel
        .notify_user(
            &Subject::new("agent"),
            &ParticipantId::new("not-a-slack-id"),
            &OutboundMessage::new("ping"),
        )
        .await
        .expect_err("invalid user id must surface");
    assert!(matches!(err, ChannelError::InvalidEnvelope(_)));
}
