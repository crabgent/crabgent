mod common;

use crabgent_channel_slack::{
    CompleteUploadFile, CompleteUploadRequest, ConversationType, SlackUserGroupId, SlackUserId,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use common::slack_test_ctx;

#[tokio::test]
async fn covers_remaining_slack_web_api_methods() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    mount_remaining_methods(server).await;
    let client = ctx.http_client_with_retry(2);

    assert_message_methods(&client, &ctx.test_channel).await;
    assert_read_methods(&client, &ctx.test_channel).await;
    assert_user_conversation_methods(&client).await;
    assert_usergroup_methods(&client).await;
    assert_reaction_file_search_methods(&client, &ctx.test_channel).await;
}

async fn mount_remaining_methods(server: &wiremock::MockServer) {
    mount_json(
        server,
        "/chat.update",
        json!({"ok": true, "channel": "C123", "ts": "2.1"}),
    )
    .await;
    mount_json(
        server,
        "/chat.delete",
        json!({"ok": true, "channel": "C123", "ts": "2.1"}),
    )
    .await;
    mount_json(
        server,
        "/conversations.replies",
        json!({"ok": true, "messages": [{"ts": "1.1", "text": "root"}]}),
    )
    .await;
    mount_json(
        server,
        "/conversations.info",
        json!({
            "ok": true,
            "channel": {
                "id": "C123",
                "is_private": true,
                "is_member": true,
                "is_im": false,
                "is_mpim": false
            }
        }),
    )
    .await;
    mount_json(
        server,
        "/conversations.members",
        json!({"ok": true, "members": ["U1", "U2"]}),
    )
    .await;
    mount_json(
        server,
        "/users.info",
        json!({"ok": true, "user": {"id": "U1", "name": "Ada"}}),
    )
    .await;
    mount_json(
        server,
        "/users.conversations",
        json!({
            "ok": true,
            "channels": [
                {"id": "C123", "is_im": false},
                {"id": "G123", "is_mpim": true}
            ]
        }),
    )
    .await;
    mount_json(
        server,
        "/usergroups.users.list",
        json!({"ok": true, "users": ["U1", "U2"]}),
    )
    .await;
    mount_json(server, "/reactions.add", json!({"ok": true})).await;
    mount_json(server, "/reactions.remove", json!({"ok": true})).await;
    mount_json(
        server,
        "/files.getUploadURLExternal",
        json!({"ok": true, "upload_url": "https://upload.example.test", "file_id": "F1"}),
    )
    .await;
    mount_json(server, "/files.completeUploadExternal", json!({"ok": true})).await;
    mount_json(
        server,
        "/search.messages",
        json!({
            "ok": true,
            "messages": {
                "matches": [{
                    "text": "hit",
                    "username": "ada",
                    "ts": "1.1",
                    "permalink": "https://example.test/archive"
                }]
            }
        }),
    )
    .await;
}

async fn assert_message_methods(client: &crabgent_channel_slack::SlackHttpClient, channel: &str) {
    assert_eq!(
        client
            .update_message(channel, "1.1", "updated")
            .await
            .expect("update")
            .ts
            .as_deref(),
        Some("2.1")
    );
    assert_eq!(
        client
            .delete_message(channel, "2.1")
            .await
            .expect("delete")
            .channel
            .as_deref(),
        Some("C123")
    );
}

async fn assert_read_methods(client: &crabgent_channel_slack::SlackHttpClient, channel: &str) {
    assert_eq!(
        client
            .conversations_replies(channel, "1.1", 10)
            .await
            .expect("replies")
            .messages
            .len(),
        1
    );
    assert!(
        client
            .conversations_info(channel)
            .await
            .expect("info")
            .channel
            .is_member
    );
    assert_eq!(
        client
            .conversations_members(channel)
            .await
            .expect("members")
            .members,
        ["U1", "U2"]
    );
    assert_eq!(
        client
            .users_info("U1")
            .await
            .expect("user")
            .user
            .name
            .as_deref(),
        Some("Ada")
    );
}

async fn assert_user_conversation_methods(client: &crabgent_channel_slack::SlackHttpClient) {
    let user = SlackUserId::new("U123").expect("user");
    let channels = client
        .users_conversations(
            &user,
            &[ConversationType::PrivateChannel, ConversationType::Mpim],
        )
        .await
        .expect("user conversations");
    assert_eq!(
        channels
            .iter()
            .map(crabgent_channel_slack::SlackChannelId::as_str)
            .collect::<Vec<_>>(),
        ["C123", "G123"]
    );
}

async fn assert_usergroup_methods(client: &crabgent_channel_slack::SlackHttpClient) {
    let group = SlackUserGroupId::new("S123").expect("user group");
    let users = client
        .usergroups_users_list(&group)
        .await
        .expect("usergroup users");
    assert_eq!(
        users.iter().map(SlackUserId::as_str).collect::<Vec<_>>(),
        ["U1", "U2"]
    );
}

async fn assert_reaction_file_search_methods(
    client: &crabgent_channel_slack::SlackHttpClient,
    channel: &str,
) {
    assert!(
        client
            .reactions_add(channel, "1.1", "eyes")
            .await
            .expect("add")
            .ok
    );
    assert!(
        client
            .reactions_remove(channel, "1.1", "eyes")
            .await
            .expect("remove")
            .ok
    );
    let upload = client
        .files_get_upload_url_external("report.txt", 10)
        .await
        .expect("upload URL");
    assert_eq!(upload.file_id, "F1");
    let complete = CompleteUploadRequest {
        files: vec![CompleteUploadFile {
            id: "F1",
            title: "report.txt",
        }],
        channel_id: channel,
        initial_comment: Some("done"),
        thread_ts: None,
    };
    assert!(
        client
            .files_complete_upload_external(&complete)
            .await
            .expect("complete")
            .ok
    );
    assert_eq!(
        client
            .search_messages("from:ada report", 20)
            .await
            .expect("search")
            .messages
            .matches
            .len(),
        1
    );
}

async fn mount_json(server: &wiremock::MockServer, request_path: &str, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path(request_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(server)
        .await;
}
