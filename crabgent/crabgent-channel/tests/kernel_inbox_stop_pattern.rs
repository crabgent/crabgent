mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::{
    ChannelError, ChannelInbox, ChannelSink, InboundEvent, KernelChannelInbox, MessageRef,
    OutboundMessage, Participant, ParticipantRole,
};
use crabgent_core::Kernel;
use crabgent_core::owner::Owner;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::subject::Subject;
use tokio::sync::Mutex;

use support::BlockingProvider;

#[derive(Default)]
struct RecordingSink {
    messages: Mutex<Vec<(Owner, OutboundMessage)>>,
}

impl RecordingSink {
    async fn messages(&self) -> Vec<(Owner, OutboundMessage)> {
        self.messages.lock().await.clone()
    }
}

#[async_trait]
impl ChannelSink for RecordingSink {
    async fn send(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        self.messages.lock().await.push((conv.clone(), msg.clone()));
        Ok(MessageRef::top_level(
            "test",
            Owner::new("test:1"),
            "test-id",
        ))
    }

    async fn react(
        &self,
        _ctx: &Subject,
        _conv: &Owner,
        _parent: &MessageRef,
        _emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        Err(ChannelError::Unsupported("react"))
    }
}

fn inbox(provider: BlockingProvider) -> KernelChannelInbox {
    let kernel = Arc::new(
        Kernel::builder()
            .provider(provider)
            .policy(AllowAllPolicy)
            .build(),
    );
    KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
}

fn event(body: &str) -> InboundEvent {
    InboundEvent {
        channel: "slack".to_owned(),
        conv: Owner::new("slack:T1/D1"),
        kind: None,
        from: Participant::new("U1", ParticipantRole::Human),
        message: MessageRef::top_level("slack", Owner::new("slack:T1/D1"), "ts:1"),
        body: body.to_owned(),
        attachments: vec![],
        timestamp: Utc::now(),
    }
}

async fn wait_in_flight(inbox: &KernelChannelInbox, expected: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if inbox.in_flight_runs().await == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("in-flight count reached expected value");
}

async fn assert_no_extra_spawn_during_window(
    inbox: &KernelChannelInbox,
    provider: &BlockingProvider,
    expected_started: usize,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(80);
    while tokio::time::Instant::now() < deadline {
        assert!(
            inbox.in_flight_runs().await <= 1,
            "only one replacement run may remain in flight"
        );
        assert_eq!(
            provider.started(),
            expected_started,
            "stale release must not allow an extra spawn"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_message_cancels_inflight_run() {
    let provider = BlockingProvider::new();
    let inbox = inbox(provider.clone());
    inbox.receive(event("work")).await.expect("receive work");
    provider.wait_started(1).await;

    inbox.receive(event("stop")).await.expect("receive stop");

    provider.wait_cancelled(1).await;
    wait_in_flight(&inbox, 0).await;
    assert!(!inbox.global_cancel_fired());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_with_ack_sink_sends_message() {
    let provider = BlockingProvider::new();
    let sink = Arc::new(RecordingSink::default());
    let ack_sink: Arc<dyn ChannelSink> = sink.clone();
    let inbox = inbox(provider.clone()).with_cancel_ack_sink(ack_sink);
    inbox.receive(event("work")).await.expect("receive work");
    provider.wait_started(1).await;

    inbox.receive(event("stop")).await.expect("receive stop");

    provider.wait_cancelled(1).await;
    let messages = sink.messages().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].0.as_str(), "slack:T1/D1");
    assert_eq!(messages[0].1.body, "Cancelled.");
    assert_eq!(
        messages[0].1.metadata.get("channel").map(String::as_str),
        Some("slack")
    );
    wait_in_flight(&inbox, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_inflight_silent_noop() {
    let provider = BlockingProvider::new();
    let inbox = inbox(provider.clone());

    inbox.receive(event("stop")).await.expect("receive stop");

    assert_eq!(provider.started(), 0);
    assert_eq!(inbox.in_flight_runs().await, 0);
    assert!(!inbox.global_cancel_fired());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn override_patterns_work() {
    let provider = BlockingProvider::new();
    let inbox = inbox(provider.clone())
        .with_stop_patterns(vec!["^halt$".to_owned()])
        .expect("override pattern should compile");
    inbox.receive(event("work")).await.expect("receive work");
    provider.wait_started(1).await;

    inbox.receive(event("halt")).await.expect("receive halt");

    provider.wait_cancelled(1).await;
    wait_in_flight(&inbox, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn case_insensitive_default() {
    for word in ["STOP", "stopp", "Cancel", "abbruch", "Halt!"] {
        let provider = BlockingProvider::new();
        let inbox = inbox(provider.clone());
        inbox.receive(event("work")).await.expect("receive work");
        provider.wait_started(1).await;

        inbox.receive(event(word)).await.expect("receive stop word");

        provider.wait_cancelled(1).await;
        wait_in_flight(&inbox, 0).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_cancel_new_run_works() {
    let provider = BlockingProvider::new();
    let inbox = inbox(provider.clone());
    inbox.receive(event("first")).await.expect("receive first");
    provider.wait_started(1).await;

    inbox.receive(event("stop")).await.expect("receive stop");
    provider.wait_cancelled(1).await;
    wait_in_flight(&inbox, 0).await;
    assert!(!inbox.global_cancel_fired());
    inbox
        .receive(event("second"))
        .await
        .expect("receive second");
    provider.wait_started(2).await;

    provider.release(1);
    provider.wait_completed(1).await;
    wait_in_flight(&inbox, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_then_immediate_new_message_does_not_double_spawn() {
    let provider = BlockingProvider::holding_cancelled();
    let inbox = inbox(provider.clone());
    inbox.receive(event("first")).await.expect("receive first");
    provider.wait_started(1).await;

    inbox.receive(event("stop")).await.expect("receive stop");
    provider.wait_cancelled(1).await;
    inbox
        .receive(event("second"))
        .await
        .expect("receive second");
    provider.wait_started(2).await;

    provider.release_cancelled(1);
    wait_in_flight(&inbox, 1).await;
    inbox.receive(event("third")).await.expect("receive third");
    assert_no_extra_spawn_during_window(&inbox, &provider, 2).await;

    provider.release(1);
    provider.wait_completed(1).await;
    wait_in_flight(&inbox, 0).await;
}

#[test]
fn invalid_regex_at_construction_fails() {
    let provider = BlockingProvider::new();
    let result = inbox(provider).with_stop_patterns(vec!["[unbalanced".to_owned()]);
    assert!(matches!(result, Err(ChannelError::InvalidPattern(_))));
}
