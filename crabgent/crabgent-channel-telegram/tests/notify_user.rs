use crabgent_channel::{Channel, ChannelError, MessageRef, OutboundMessage, ParticipantId};
use crabgent_channel_telegram::TelegramChannel;
use crabgent_core::owner::Owner;
use crabgent_core::subject::Subject;
use httpmock::Method::POST;
use httpmock::MockServer;
use serde_json::json;

fn build_channel(server: &MockServer) -> TelegramChannel {
    TelegramChannel::new("test-token", "B-1", "crabgent_bot").with_api_base(server.base_url())
}

#[tokio::test]
async fn notify_user_posts_send_message_to_recipient_chat_id() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/sendMessage")
            .json_body(json!({
                "chat_id": 99,
                "text": "ping <code>ok</code>",
                "parse_mode": "HTML"
            }));
        then.status(200).json_body(json!({
            "ok": true,
            "result": {
                "message_id": 1801,
                "chat": {"id": 99}
            }
        }));
    });
    let channel = build_channel(&server);
    let ctx = Subject::new("agent");
    let recipient = ParticipantId::new("user:99");

    let sent = channel
        .notify_user(
            &ctx,
            &recipient,
            &OutboundMessage::new("ping <code>ok</code>"),
        )
        .await
        .expect("telegram notify_user should send message");

    mock.assert();
    assert_eq!(sent.channel, "telegram");
    assert_eq!(sent.conv.as_str(), "telegram:99");
    assert_eq!(sent.id, "1801");
    assert!(sent.thread_root.is_none());
}

#[tokio::test]
async fn notify_user_accepts_bare_numeric_recipient() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/sendMessage")
            .json_body(json!({
                "chat_id": 12345,
                "text": "hi"
            }));
        then.status(200).json_body(json!({
            "ok": true,
            "result": {
                "message_id": 2,
                "chat": {"id": 12345}
            }
        }));
    });
    let channel = build_channel(&server);
    let ctx = Subject::new("agent");
    let recipient = ParticipantId::new("12345");

    let sent = channel
        .notify_user(&ctx, &recipient, &OutboundMessage::new("hi"))
        .await
        .expect("telegram notify_user should accept numeric recipient");
    assert_eq!(sent.conv.as_str(), "telegram:12345");
}

#[tokio::test]
async fn notify_user_ignores_thread_parent() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/sendMessage")
            .json_body(json!({
                "chat_id": 99,
                "text": "top-level"
            }));
        then.status(200).json_body(json!({
            "ok": true,
            "result": {
                "message_id": 1803,
                "chat": {"id": 99}
            }
        }));
    });
    let channel = build_channel(&server);
    let ctx = Subject::new("agent");
    let parent = MessageRef::thread_reply("telegram", Owner::new("telegram:99"), "1700", "99");

    let sent = channel
        .notify_user(
            &ctx,
            &ParticipantId::new("user:99"),
            &OutboundMessage::new("top-level").in_thread(parent),
        )
        .await
        .expect("telegram notify_user should send top-level message");

    mock.assert();
    assert_eq!(sent.id, "1803");
    assert!(sent.thread_root.is_none());
}

#[tokio::test]
async fn notify_user_normalizes_markdown_body() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/sendMessage")
            .json_body(json!({
                "chat_id": 99,
                "text": "<b>ping</b> <a href=\"https://example.com\">site</a>",
                "parse_mode": "HTML"
            }));
        then.status(200).json_body(json!({
            "ok": true,
            "result": {
                "message_id": 1802,
                "chat": {"id": 99}
            }
        }));
    });
    let channel = build_channel(&server);
    let ctx = Subject::new("agent");

    let sent = channel
        .notify_user(
            &ctx,
            &ParticipantId::new("user:99"),
            &OutboundMessage::new("**ping** [site](https://example.com)"),
        )
        .await
        .expect("telegram notify_user should send message");

    mock.assert();
    assert_eq!(sent.id, "1802");
}

#[tokio::test]
async fn notify_user_maps_cannot_initiate_to_adapter_error() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottest-token/sendMessage");
        then.status(403).json_body(json!({
            "ok": false,
            "error_code": 403,
            "description": "Forbidden: bot can't initiate conversation with a user"
        }));
    });
    let channel = build_channel(&server);
    let ctx = Subject::new("agent");
    let recipient = ParticipantId::new("user:99");

    let err = channel
        .notify_user(&ctx, &recipient, &OutboundMessage::new("ping"))
        .await
        .expect_err("telegram 403 must surface as an adapter error");

    let message = adapter_message(err).expect("expected ChannelError::Adapter");
    assert!(
        message.contains("403"),
        "adapter error should retain the HTTP 403 status, got: {message}"
    );
}

fn adapter_message(err: ChannelError) -> Option<String> {
    match err {
        ChannelError::Adapter(message) => Some(message),
        _ => None,
    }
}

#[tokio::test]
async fn notify_user_rejects_non_numeric_recipient_before_dispatch() {
    let server = MockServer::start();
    let channel = build_channel(&server);
    let ctx = Subject::new("agent");
    let recipient = ParticipantId::new("@alice:example.org");

    let err = channel
        .notify_user(&ctx, &recipient, &OutboundMessage::new("hi"))
        .await
        .expect_err("non-numeric recipient must fail");

    assert!(matches!(err, ChannelError::InvalidEnvelope(_)));
}
