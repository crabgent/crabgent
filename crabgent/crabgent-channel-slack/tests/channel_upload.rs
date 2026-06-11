mod common;

use crabgent_channel::{Channel, ChannelError, MessageRef};
use crabgent_channel_slack::SlackChannel;
use crabgent_core::{owner::Owner, subject::Subject};
use wiremock::matchers::{body_json, body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

use common::{slack_client, slack_test_ctx};

#[tokio::test]
async fn channel_upload_uses_external_upload_flow() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_upload_flow(server, true).await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let conv = Owner::new("slack:T123/C123");
    let parent = MessageRef::thread_reply("slack", conv.clone(), "1.2", "1.1");

    let message = channel
        .upload(
            &Subject::new("agent"),
            &conv,
            "note.txt",
            b"hello".to_vec(),
            Some("**caption**"),
            Some(&parent),
        )
        .await
        .expect("upload");

    assert_eq!(message.id, "F1");
    assert_eq!(message.thread_root(), Some("1.1"));
}

#[tokio::test]
async fn partial_failure_complete_upload_error_after_put_success() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_upload_flow(server, false).await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let conv = Owner::new("slack:T123/C123");
    let parent = MessageRef::top_level("slack", conv.clone(), "1.1");

    let err = channel
        .upload(
            &Subject::new("agent"),
            &conv,
            "note.txt",
            b"hello".to_vec(),
            Some("**caption**"),
            Some(&parent),
        )
        .await
        .expect_err("complete failure");

    // `ChannelError::Adapter` Display is opaque for LLM-safety; the
    // underlying Slack-API code is preserved in the inner field for
    // Debug + tracing, so destructure and assert against the field.
    let ChannelError::Adapter(detail) = &err else {
        panic!("expected Adapter, got {err:?}");
    };
    assert!(detail.contains("file_not_found"), "{detail}");
}

async fn mount_upload_flow(server: &wiremock::MockServer, complete_ok: bool) {
    let upload_url = format!("{}/upload", server.uri());
    Mock::given(method("POST"))
        .and(path("/files.getUploadURLExternal"))
        .and(body_string_contains("filename=note.txt"))
        .and(body_string_contains("length=5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "upload_url": upload_url,
            "file_id": "F1"
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
    let complete_body = if complete_ok {
        serde_json::json!({"ok": true})
    } else {
        serde_json::json!({"ok": false, "error": "file_not_found"})
    };
    Mock::given(method("POST"))
        .and(path("/files.completeUploadExternal"))
        .and(body_json(serde_json::json!({
            "files": [{"id": "F1", "title": "note.txt"}],
            "channel_id": "C123",
            "initial_comment": "*caption*",
            "thread_ts": "1.1"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(complete_body))
        .mount(server)
        .await;
}
