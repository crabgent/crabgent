use std::sync::Arc;
use std::time::Duration;

use crabgent_core::Kernel;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::types::ToolResult;
use serde_json::json;

use crate::channel::ChannelKind;
use crate::inbox::ChannelInbox;
use crate::participant::ParticipantRole;
use crate::test_support::RecordingChannel;
use crate::tools::ChannelSendTool;

use super::build_event;
use super::live_turn_support::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_text_is_sent_without_channel_send_tool() {
    let channel = Arc::new(RecordingChannel::new("stub", ChannelKind::Direct, "m1"));
    let sink = sink_for(&channel);
    let provider = ScriptedProvider::new([text_response("plain answer")]);
    let inbox = inbox_with_sink(kernel_with_provider(provider), sink);

    inbox
        .receive(build_event("stub", "stub:c", ParticipantRole::Human, "hi"))
        .await
        .expect("receive ok");

    wait_for_sent(&channel, 1).await;
    assert_eq!(
        channel.last_sent().expect("recorded send").body,
        "plain answer"
    );
    assert_eq!(channel.edit_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_final_text_is_reported_without_channel_side_effect() {
    let channel = Arc::new(RecordingChannel::new("stub", ChannelKind::Direct, "m1"));
    let sink = sink_for(&channel);
    let provider = ScriptedProvider::new([text_response("")]);
    let inbox = inbox_with_sink(kernel_with_provider(provider), sink);

    inbox
        .receive(build_event("stub", "stub:c", ParticipantRole::Human, "hi"))
        .await
        .expect("receive ok");

    wait_for_sent(&channel, 1).await;
    assert_eq!(
        channel.last_sent().expect("recorded send").body,
        "No response produced."
    );
    assert_eq!(channel.edit_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_channel_send_suppresses_duplicate_final() {
    let channel = Arc::new(RecordingChannel::new("stub", ChannelKind::Direct, "m1"));
    let sink = sink_for(&channel);
    let provider = ScriptedProvider::new([
        tool_response(vec![call(
            "channel_send",
            json!({"conv": "stub:c", "body": "tool answer"}),
        )]),
        text_response("duplicate final"),
    ]);
    let tool = ChannelSendTool::new(Arc::clone(&sink), Arc::new(AllowAllPolicy));
    let kernel = Arc::new(
        Kernel::builder()
            .provider(provider)
            .add_tool(tool)
            .policy(AllowAllPolicy)
            .build(),
    );
    let inbox = inbox_with_sink(kernel, sink);

    inbox
        .receive(build_event("stub", "stub:c", ParticipantRole::Human, "hi"))
        .await
        .expect("receive ok");

    wait_for_sent(&channel, 1).await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(channel.sent_count(), 1);
    assert_eq!(
        channel.last_sent().expect("recorded send").body,
        "tool answer"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_channel_send_reports_sanitized_delivery_failure() {
    let channel = Arc::new(RecordingChannel::new("stub", ChannelKind::Direct, "m1"));
    let sink = sink_for(&channel);
    let provider = ScriptedProvider::new([
        tool_response(vec![call("channel_send", json!({}))]),
        text_response(""),
    ]);
    let tool = StaticTool {
        name: "channel_send",
        result: ToolResult::soft_error(json!("adapter\nfailed with a very long reason")),
    };
    let kernel = Arc::new(
        Kernel::builder()
            .provider(provider)
            .add_tool(tool)
            .policy(AllowAllPolicy)
            .build(),
    );
    let inbox = inbox_with_sink(kernel, sink);

    inbox
        .receive(build_event("stub", "stub:c", ParticipantRole::Human, "hi"))
        .await
        .expect("receive ok");

    wait_for_sent(&channel, 1).await;
    let body = channel.last_sent().expect("recorded send").body;
    assert_eq!(
        body,
        "Delivery failed: adapter failed with a very long reason"
    );
    assert!(!body.contains('\n'));
}
