//! Live-turn progress, reasoning, cancel-ack, and formatting-hint delivery
//! tests. Split out of `live_turn.rs` to keep each file under the LOC cap.

use std::sync::Arc;
use std::time::Duration;

use crabgent_core::Kernel;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::types::ToolResult;
use serde_json::json;

use crate::channel::ChannelKind;
use crate::inbox::{ChannelInbox, KernelChannelInbox, LiveProgressMode, LiveTurnConfig};
use crate::participant::ParticipantRole;
use crate::test_support::RecordingChannel;

use super::build_event;
use super::live_turn_support::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_uses_one_message_and_edits_across_tool_events() {
    let channel = Arc::new(RecordingChannel::new("stub", ChannelKind::Direct, "m1"));
    let sink = sink_for(&channel);
    let provider = ScriptedProvider::new([
        tool_response(vec![call("bash", json!({})), call("search", json!({}))]),
        text_response("all done"),
    ]);
    let kernel = Arc::new(
        Kernel::builder()
            .provider(provider)
            .add_tool(StaticTool {
                name: "bash",
                result: ToolResult::success(json!("ok")),
            })
            .add_tool(StaticTool {
                name: "search",
                result: ToolResult::success(json!("ok")),
            })
            .policy(AllowAllPolicy)
            .build(),
    );
    let config = LiveTurnConfig::default().with_edit_throttle(Duration::ZERO);
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
        .with_live_turn_delivery_config(sink, config);

    inbox
        .receive(build_event("stub", "stub:c", ParticipantRole::Human, "hi"))
        .await
        .expect("receive ok");

    wait_for_sent(&channel, 1).await;
    wait_for_edit(&channel, 3).await;
    assert_eq!(channel.sent_count(), 1);
    assert_eq!(
        channel.last_sent().expect("recorded send").body,
        "Using bash..."
    );
    assert_eq!(channel.last_edit().expect("recorded edit").1, "all done");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reasoning_progress_does_not_leak_raw_reasoning_text() {
    let raw = "raw chain-of-thought secret";
    let channel = Arc::new(RecordingChannel::new("stub", ChannelKind::Direct, "m1"));
    let sink = sink_for(&channel);
    let kernel = kernel_with_provider(ReasoningProvider { raw_reasoning: raw });
    let config = LiveTurnConfig::default().with_progress_mode(LiveProgressMode::Eager);
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
        .with_live_turn_delivery_config(sink, config);

    inbox
        .receive(build_event("stub", "stub:c", ParticipantRole::Human, "hi"))
        .await
        .expect("receive ok");

    wait_for_sent(&channel, 1).await;
    wait_for_edit(&channel, 1).await;
    let sent = channel.last_sent().expect("recorded send").body;
    let edited = channel.last_edit().expect("recorded edit").1;
    assert_eq!(sent, "Working...");
    assert_eq!(edited, "final answer");
    assert!(!sent.contains(raw));
    assert!(!edited.contains(raw));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_pattern_ack_is_not_duplicated_by_live_turn_status() {
    let provider = BlockingProvider::new();
    let channel = Arc::new(RecordingChannel::new("stub", ChannelKind::Direct, "m1"));
    let sink = sink_for(&channel);
    let kernel = kernel_with_provider(provider.clone());
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
        .with_live_turn_delivery(Arc::clone(&sink))
        .with_cancel_ack_sink(sink);

    inbox
        .receive(build_event(
            "stub",
            "stub:c",
            ParticipantRole::Human,
            "work",
        ))
        .await
        .expect("receive work");
    provider.wait_started().await;

    inbox
        .receive(build_event(
            "stub",
            "stub:c",
            ParticipantRole::Human,
            "stop",
        ))
        .await
        .expect("receive stop");
    wait_for_sent(&channel, 1).await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(channel.sent_count(), 1);
    assert_eq!(
        channel.last_sent().expect("recorded send").body,
        "Cancelled."
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn formatting_hint_is_present_for_final_delivery_runs() {
    let channel = Arc::new(RecordingChannel::new("stub", ChannelKind::Direct, "m1"));
    let sink = sink_for(&channel);
    let provider = ScriptedProvider::new([text_response("formatted answer")]);
    let provider_handle = provider.clone();
    let inbox =
        inbox_with_sink(kernel_with_provider(provider), sink).with_formatting_hint("FORMAT HINT");

    inbox
        .receive(build_event("stub", "stub:c", ParticipantRole::Human, "hi"))
        .await
        .expect("receive ok");

    wait_for_sent(&channel, 1).await;
    assert_eq!(
        channel.last_sent().expect("recorded send").body,
        "formatted answer"
    );
    let prompts = provider_handle.seen_prompts();
    assert!(
        prompts
            .iter()
            .flatten()
            .any(|prompt| prompt.contains("FORMAT HINT")),
        "formatting hint should reach the provider request"
    );
    let prompt = prompts
        .iter()
        .flatten()
        .next()
        .expect("provider saw system prompt");
    assert!(
        prompt.contains("Reply by writing normal assistant text in your final response"),
        "live delivery hint should tell the model that final text is delivered: {prompt:?}"
    );
    assert!(
        !prompt.contains("Plain text in your final response is NOT delivered"),
        "live delivery hint must not contain stale channel_send-only guidance: {prompt:?}"
    );
}
