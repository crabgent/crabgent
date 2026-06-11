mod common;

use std::sync::Arc;
use std::time::Duration;

use crabgent_channel::channel::ChannelKind;
use crabgent_channel::subject::ChannelSubjectExt;
use crabgent_channel_slack::subject::{SLACK_CHANNEL_ID, SLACK_THREAD_ROOT};
use crabgent_channel_slack::{
    MarkdownTextChunk, ProgressChunk, SlackAgentProgress, SlackAgentProgressIndicator, SlackAppType,
};
use crabgent_core::owner::Owner;
use crabgent_core::{RunCtx, RunId, Subject};
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{SlackTestCtx, slack_test_ctx};

const CHANNEL: &str = "C1";
const THREAD: &str = "1700000000.000100";
const STREAM_CHANNEL: &str = "C42";
const STREAM_TS: &str = "1700000000.999000";

async fn mount_json(server: &MockServer, slack_path: &str, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path(slack_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn mount_json_expect(
    server: &MockServer,
    slack_path: &str,
    body: serde_json::Value,
    expected_hits: u64,
) {
    Mock::given(method("POST"))
        .and(path(slack_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(expected_hits)
        .mount(server)
        .await;
}

async fn mount_json_expect_body(
    server: &MockServer,
    slack_path: &str,
    expected_body: serde_json::Value,
    response_body: serde_json::Value,
    expected_hits: u64,
) {
    Mock::given(method("POST"))
        .and(path(slack_path))
        .and(body_partial_json(expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .expect(expected_hits)
        .mount(server)
        .await;
}

async fn mount_status_expect(
    server: &MockServer,
    http_status: u16,
    body: serde_json::Value,
    expected_hits: u64,
) {
    Mock::given(method("POST"))
        .and(path("/assistant.threads.setStatus"))
        .respond_with(ResponseTemplate::new(http_status).set_body_json(body))
        .expect(expected_hits)
        .mount(server)
        .await;
}

fn slack_subject() -> Subject {
    Subject::new("agent")
        .with_channel("slack", &Owner::new("slack:T1/C1"), ChannelKind::Group)
        .with_attr(SLACK_CHANNEL_ID, CHANNEL)
        .with_attr(SLACK_THREAD_ROOT, THREAD)
}

fn slack_ctx() -> RunCtx {
    RunCtx::new(RunId::new(), slack_subject())
}

fn slack_ctx_with_id(run_id: RunId) -> RunCtx {
    RunCtx::new(run_id, slack_subject())
}

fn indicator_from(ctx: &SlackTestCtx) -> SlackAgentProgressIndicator {
    SlackAgentProgressIndicator::new(Arc::new(ctx.http_client_with_retry(0)))
}

#[tokio::test]
async fn indicator_debug_omits_client_state() {
    let ctx = slack_test_ctx().await;
    let indicator = indicator_from(&ctx);

    let rendered = format!("{indicator:?}");

    assert!(rendered.starts_with("SlackAgentProgressIndicator"));
    assert!(rendered.contains("app_type"));
    assert!(rendered.contains(".."));
}

#[tokio::test]
async fn v2_setstatus_happy_promotes_to_ai_agent() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_json(server, "/assistant.threads.setStatus", json!({"ok": true})).await;

    let indicator = indicator_from(&ctx);
    let rc = slack_ctx();
    indicator.start(&rc, "thinking...").await.expect("start ok");

    assert_eq!(indicator.app_type(), SlackAppType::AiAgent);
    assert_eq!(indicator.active_runs_count(), 1);
}

#[tokio::test]
async fn v2_setstatus_sentinel_caches_standard_and_no_op_next() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_status_expect(
        server,
        200,
        json!({"ok": false, "error": "not_allowed_token_type"}),
        1,
    )
    .await;

    let indicator = indicator_from(&ctx);
    indicator
        .start(&slack_ctx(), "thinking...")
        .await
        .expect("first start ok");
    assert_eq!(indicator.app_type(), SlackAppType::Standard);
    assert_eq!(indicator.active_runs_count(), 0);

    indicator
        .start(&slack_ctx(), "thinking again")
        .await
        .expect("second start ok");
    assert_eq!(indicator.app_type(), SlackAppType::Standard);
    assert_eq!(indicator.active_runs_count(), 0);
}

#[tokio::test]
async fn v2_setstatus_transient_leaves_unknown() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_status_expect(
        server,
        500,
        json!({"ok": false, "error": "internal_error"}),
        1,
    )
    .await;

    let indicator = indicator_from(&ctx);
    let rc = slack_ctx();
    indicator.start(&rc, "thinking...").await.expect("start ok");

    assert_eq!(indicator.app_type(), SlackAppType::Unknown);
    assert_eq!(indicator.active_runs_count(), 0);

    indicator
        .chunk(&rc, ProgressChunk::Status("calling search".into()))
        .await
        .expect("chunk ok");
}

#[tokio::test]
async fn v3_chat_streaming_happy() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_json(server, "/assistant.threads.setStatus", json!({"ok": true})).await;
    mount_json_expect_body(
        server,
        "/chat.startStream",
        json!({
            "task_display_mode": "plan",
            "chunks": [
                {"type": "plan_update", "title": "thinking..."}
            ]
        }),
        json!({"ok": true, "channel": STREAM_CHANNEL, "ts": STREAM_TS}),
        1,
    )
    .await;
    mount_json_expect(server, "/chat.appendStream", json!({"ok": true}), 1).await;
    mount_json(server, "/chat.stopStream", json!({"ok": true})).await;

    let indicator = indicator_from(&ctx);
    let rc = slack_ctx();

    indicator.start(&rc, "thinking...").await.expect("start ok");
    assert_eq!(indicator.app_type(), SlackAppType::AiAgent);
    indicator
        .chunk(&rc, ProgressChunk::Status("calling search".into()))
        .await
        .expect("status chunk ok");

    let first = ProgressChunk::MarkdownText(MarkdownTextChunk {
        text: "hello".into(),
    });
    indicator.chunk(&rc, first).await.expect("first chunk ok");

    let second = ProgressChunk::MarkdownText(MarkdownTextChunk {
        text: "world".into(),
    });
    indicator.chunk(&rc, second).await.expect("second chunk ok");

    let third = ProgressChunk::MarkdownText(MarkdownTextChunk {
        text: "again".into(),
    });
    indicator.chunk(&rc, third).await.expect("third chunk ok");

    indicator.stop(&rc).await.expect("stop ok");
    assert_eq!(indicator.active_runs_count(), 0);
}

#[tokio::test]
async fn v3_startstream_sentinel_caches_standard() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_json(server, "/assistant.threads.setStatus", json!({"ok": true})).await;
    mount_json_expect(
        server,
        "/chat.startStream",
        json!({"ok": false, "error": "feature_not_supported"}),
        1,
    )
    .await;

    let indicator = indicator_from(&ctx);
    let rc = slack_ctx();

    indicator.start(&rc, "thinking...").await.expect("start ok");
    assert_eq!(indicator.app_type(), SlackAppType::AiAgent);

    let chunk = ProgressChunk::MarkdownText(MarkdownTextChunk {
        text: "first".into(),
    });
    indicator.chunk(&rc, chunk).await.expect("first chunk ok");
    tokio::time::timeout(Duration::from_secs(1), async {
        while indicator.app_type() != SlackAppType::Standard || indicator.active_runs_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("sentinel demotion");
    assert_eq!(indicator.app_type(), SlackAppType::Standard);
    assert_eq!(
        indicator.active_runs_count(),
        0,
        "sentinel demotion must abort the active heartbeat"
    );

    let next = ProgressChunk::MarkdownText(MarkdownTextChunk {
        text: "second".into(),
    });
    indicator
        .chunk(&rc, next)
        .await
        .expect("second chunk no-op");
}

#[tokio::test]
async fn non_slack_channel_no_op() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(0_u64)
        .mount(server)
        .await;

    let indicator = indicator_from(&ctx);
    let subject = Subject::new("agent")
        .with_channel(
            "matrix",
            &Owner::new("matrix:server/room"),
            ChannelKind::Group,
        )
        .with_attr(SLACK_CHANNEL_ID, CHANNEL)
        .with_attr(SLACK_THREAD_ROOT, THREAD);
    let rc = RunCtx::new(RunId::new(), subject);

    indicator.start(&rc, "thinking...").await.expect("start ok");
    assert_eq!(indicator.app_type(), SlackAppType::Unknown);
    assert_eq!(indicator.active_runs_count(), 0);
}

#[tokio::test]
async fn non_slack_channel_chunk_no_op() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_json_expect(
        server,
        "/assistant.threads.setStatus",
        json!({"ok": true}),
        1,
    )
    .await;
    // chat.startStream is fired eagerly by the consumer task spawned in
    // start() for the slack subject. Mount it without a count so the test
    // focuses on the foreign-subject no-op path.
    mount_json(
        server,
        "/chat.startStream",
        json!({"ok": true, "channel": STREAM_CHANNEL, "ts": STREAM_TS}),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/chat.appendStream"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(0_u64)
        .mount(server)
        .await;

    let indicator = indicator_from(&ctx);
    indicator
        .start(&slack_ctx(), "thinking...")
        .await
        .expect("slack start ok");
    assert_eq!(indicator.app_type(), SlackAppType::AiAgent);

    let foreign = Subject::new("agent")
        .with_channel(
            "matrix",
            &Owner::new("matrix:server/room"),
            ChannelKind::Group,
        )
        .with_attr(SLACK_CHANNEL_ID, CHANNEL)
        .with_attr(SLACK_THREAD_ROOT, THREAD);
    let foreign_ctx = RunCtx::new(RunId::new(), foreign);

    indicator
        .chunk(&foreign_ctx, ProgressChunk::Status("foreign status".into()))
        .await
        .expect("status chunk no-op");
    indicator
        .chunk(
            &foreign_ctx,
            ProgressChunk::MarkdownText(MarkdownTextChunk {
                text: "foreign markdown".into(),
            }),
        )
        .await
        .expect("markdown chunk no-op");
}

#[tokio::test]
async fn missing_thread_ts_no_op() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(0_u64)
        .mount(server)
        .await;

    let indicator = indicator_from(&ctx);
    let subject = Subject::new("agent")
        .with_channel("slack", &Owner::new("slack:T1/C1"), ChannelKind::Group)
        .with_attr(SLACK_CHANNEL_ID, CHANNEL);
    let rc = RunCtx::new(RunId::new(), subject);

    indicator.start(&rc, "thinking...").await.expect("start ok");
    assert_eq!(indicator.active_runs_count(), 0);
}

#[tokio::test]
async fn double_start_replaces_handle() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_json(server, "/assistant.threads.setStatus", json!({"ok": true})).await;

    let indicator = indicator_from(&ctx);
    let id = RunId::new();
    indicator
        .start(&slack_ctx_with_id(id.clone()), "thinking...")
        .await
        .expect("first start ok");
    assert_eq!(indicator.active_runs_count(), 1);
    indicator
        .start(&slack_ctx_with_id(id), "thinking again")
        .await
        .expect("second start ok");
    assert_eq!(
        indicator.active_runs_count(),
        1,
        "second start must replace, not add"
    );
}

#[tokio::test]
async fn stop_idempotent() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(0_u64)
        .mount(server)
        .await;

    let indicator = indicator_from(&ctx);
    indicator
        .stop(&slack_ctx())
        .await
        .expect("stop ok without prior start");
    assert_eq!(indicator.active_runs_count(), 0);
}

#[tokio::test]
async fn stop_finalises_stream_even_when_slack_target_lost() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_json(server, "/assistant.threads.setStatus", json!({"ok": true})).await;
    mount_json(
        server,
        "/chat.startStream",
        json!({"ok": true, "channel": STREAM_CHANNEL, "ts": STREAM_TS}),
    )
    .await;
    mount_json(server, "/chat.appendStream", json!({"ok": true})).await;
    mount_json_expect(server, "/chat.stopStream", json!({"ok": true}), 1).await;

    let indicator = indicator_from(&ctx);
    let id = RunId::new();
    indicator
        .start(&slack_ctx_with_id(id.clone()), "thinking...")
        .await
        .expect("start ok");
    indicator
        .chunk(
            &slack_ctx_with_id(id.clone()),
            ProgressChunk::MarkdownText(MarkdownTextChunk {
                text: "open stream".into(),
            }),
        )
        .await
        .expect("chunk ok");

    // Stop ctx carries the same RunId but no slack target attrs anymore.
    // The stream-close path must still run against the stored StreamHandle.
    let stripped =
        Subject::new("agent").with_channel("slack", &Owner::new("slack:T1/C1"), ChannelKind::Group);
    let stripped_ctx = RunCtx::new(id, stripped);
    indicator.stop(&stripped_ctx).await.expect("stop ok");
    assert_eq!(indicator.active_runs_count(), 0);
}

#[tokio::test]
async fn drop_aborts_active_handles() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_json(server, "/assistant.threads.setStatus", json!({"ok": true})).await;

    let indicator = indicator_from(&ctx);
    indicator
        .start(&slack_ctx(), "thinking...")
        .await
        .expect("start ok");
    assert_eq!(indicator.active_runs_count(), 1);
    drop(indicator);
}
