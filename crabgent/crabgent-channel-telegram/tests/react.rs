use crabgent_channel::{Channel, ChannelError, MessageRef};
use crabgent_channel_telegram::TelegramChannel;
use crabgent_core::{owner::Owner, subject::Subject};
use httpmock::Method::POST;
use httpmock::MockServer;
use serde_json::json;

fn build_channel(server: &MockServer) -> TelegramChannel {
    TelegramChannel::new("test-token", "B-1", "crabgent_bot").with_api_base(server.base_url())
}

#[tokio::test]
async fn react_dispatches_set_message_reaction_payload() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/setMessageReaction")
            .json_body(json!({
                "chat_id": 42,
                "message_id": 1700,
                "reaction": [{
                    "type": "emoji",
                    "emoji": "👀"
                }]
            }));
        then.status(200)
            .json_body(json!({"ok": true, "result": true}));
    });
    let channel = build_channel(&server);
    let ctx = Subject::new("agent");
    let conv = Owner::new("telegram:42");
    let parent = MessageRef::top_level("telegram", conv.clone(), "1700");

    let sent = channel
        .react(&ctx, &conv, &parent, "👀")
        .await
        .expect("telegram reaction should be sent");

    mock.assert();
    assert_eq!(sent.channel, "telegram");
    assert_eq!(sent.conv, conv);
    assert_eq!(sent.id, "1700");
    assert!(sent.thread_root.is_none());
}

#[tokio::test]
async fn react_rejects_non_numeric_parent_id_before_dispatch() {
    let server = MockServer::start();
    let channel = build_channel(&server);
    let ctx = Subject::new("agent");
    let conv = Owner::new("telegram:42");
    let parent = MessageRef::top_level("telegram", conv.clone(), "abc");

    let err = channel
        .react(&ctx, &conv, &parent, "👀")
        .await
        .expect_err("non-numeric telegram parent id must fail");

    assert!(matches!(err, ChannelError::InvalidEnvelope(_)));
}
