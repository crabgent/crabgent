mod common;

use crabgent_channel_slack::{
    BlocksChunk, MarkdownTextChunk, PlanUpdateChunk, SlackError, SlackHttpClient, StreamChunk,
    TaskStatus, TaskUpdateChunk,
};
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::slack_test_ctx;

const TEST_CHANNEL: &str = "C1";
const TEST_THREAD_TS: &str = "1700000000.001000";

async fn mount_json(server: &MockServer, slack_path: &str, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path(slack_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn mount_json_with_body(
    server: &MockServer,
    slack_path: &str,
    expected_body: serde_json::Value,
    response_body: serde_json::Value,
) {
    Mock::given(method("POST"))
        .and(path(slack_path))
        .and(body_partial_json(expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn assistant_threads_set_status_happy_returns_ok() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    mount_json_with_body(
        server,
        "/assistant.threads.setStatus",
        json!({
            "channel_id": TEST_CHANNEL,
            "thread_ts": TEST_THREAD_TS,
            "status": "thinking..."
        }),
        json!({"ok": true}),
    )
    .await;

    let client: SlackHttpClient = ctx.http_client_with_retry(0);
    client
        .assistant_threads_set_status(TEST_CHANNEL, TEST_THREAD_TS, "thinking...")
        .await
        .expect("set status ok");
}

#[tokio::test]
async fn assistant_threads_set_status_feature_not_supported_returns_api_error() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    mount_json(
        server,
        "/assistant.threads.setStatus",
        json!({"ok": false, "error": "feature_not_supported"}),
    )
    .await;

    let client = ctx.http_client_with_retry(0);
    let err = client
        .assistant_threads_set_status(TEST_CHANNEL, TEST_THREAD_TS, "thinking...")
        .await
        .expect_err("expected sentinel error");
    match err {
        SlackError::ApiError {
            slack_code,
            http_status,
        } => {
            assert_eq!(slack_code, "feature_not_supported");
            assert_eq!(http_status, Some(200));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn assistant_threads_set_status_not_allowed_token_type_returns_api_error() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    mount_json(
        server,
        "/assistant.threads.setStatus",
        json!({"ok": false, "error": "not_allowed_token_type"}),
    )
    .await;

    let client = ctx.http_client_with_retry(0);
    let err = client
        .assistant_threads_set_status(TEST_CHANNEL, TEST_THREAD_TS, "thinking...")
        .await
        .expect_err("expected sentinel error");
    match err {
        SlackError::ApiError { slack_code, .. } => {
            assert_eq!(slack_code, "not_allowed_token_type");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn assistant_threads_set_status_empty_status_clears() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    mount_json_with_body(
        server,
        "/assistant.threads.setStatus",
        json!({
            "channel_id": TEST_CHANNEL,
            "thread_ts": TEST_THREAD_TS,
            "status": ""
        }),
        json!({"ok": true}),
    )
    .await;

    let client = ctx.http_client_with_retry(0);
    client
        .assistant_threads_set_status(TEST_CHANNEL, TEST_THREAD_TS, "")
        .await
        .expect("clear status ok");
}

#[tokio::test]
async fn chat_start_stream_returns_stream_handle() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    let chunks = vec![
        StreamChunk::PlanUpdate(PlanUpdateChunk {
            title: "thinking...".to_owned(),
        }),
        StreamChunk::MarkdownText(MarkdownTextChunk {
            text: "hello".to_owned(),
        }),
    ];
    mount_json_with_body(
        server,
        "/chat.startStream",
        json!({
            "channel": TEST_CHANNEL,
            "thread_ts": TEST_THREAD_TS,
            "task_display_mode": "plan",
            "chunks": [
                {"type": "plan_update", "title": "thinking..."},
                {"type": "markdown_text", "text": "hello"}
            ]
        }),
        json!({"ok": true, "channel": "C42", "ts": "1700000000.123456"}),
    )
    .await;

    let client = ctx.http_client_with_retry(0);
    let handle = client
        .chat_start_stream(TEST_CHANNEL, TEST_THREAD_TS, Some("plan"), &chunks)
        .await
        .expect("start stream ok");
    assert_eq!(handle.channel, "C42");
    assert_eq!(handle.ts, "1700000000.123456");
}

#[tokio::test]
async fn chat_append_stream_forwards_markdown_and_chunks() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    let chunks = vec![
        StreamChunk::TaskUpdate(TaskUpdateChunk {
            id: "t1".to_owned(),
            title: "search".to_owned(),
            status: TaskStatus::InProgress,
            details: None,
            output: None,
            sources: None,
        }),
        StreamChunk::PlanUpdate(PlanUpdateChunk {
            title: "plan".to_owned(),
        }),
    ];
    mount_json_with_body(
        server,
        "/chat.appendStream",
        json!({
            "channel": TEST_CHANNEL,
            "ts": "1700000000.123456",
            "markdown_text": "tail",
            "chunks": [
                {"type": "task_update", "id": "t1", "title": "search", "status": "in_progress"},
                {"type": "plan_update", "title": "plan"}
            ]
        }),
        json!({"ok": true}),
    )
    .await;

    let client = ctx.http_client_with_retry(0);
    client
        .chat_append_stream(TEST_CHANNEL, "1700000000.123456", Some("tail"), &chunks)
        .await
        .expect("append stream ok");
}

#[tokio::test]
async fn chat_stop_stream_finalises_with_blocks() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    let chunks = vec![StreamChunk::Blocks(BlocksChunk {
        blocks: json!([{"type": "section", "text": {"type": "mrkdwn", "text": "done"}}]),
    })];
    mount_json_with_body(
        server,
        "/chat.stopStream",
        json!({
            "channel": TEST_CHANNEL,
            "ts": "1700000000.123456",
            "chunks": [{
                "type": "blocks",
                "blocks": [{"type": "section", "text": {"type": "mrkdwn", "text": "done"}}]
            }]
        }),
        json!({"ok": true}),
    )
    .await;

    let client = ctx.http_client_with_retry(0);
    client
        .chat_stop_stream(TEST_CHANNEL, "1700000000.123456", &chunks)
        .await
        .expect("stop stream ok");
}
