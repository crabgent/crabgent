use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_core::error::ProviderError;
use crabgent_core::owner::Owner;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::provider::Provider;
use crabgent_core::{Kernel, ModelInfo, RunCtx};
use tokio_util::sync::CancellationToken;

use crate::envelope::{MessageRef, OutboundMessage};
use crate::error::ChannelError;
use crate::inbox::{ChannelInbox, KernelChannelInbox};
use crate::participant::ParticipantRole;
use crate::sink::ChannelSink;

use super::{build_event, build_kernel};

struct RecordingSink {
    sends: Arc<Mutex<Vec<OutboundMessage>>>,
    reactions: Arc<Mutex<Vec<(Owner, MessageRef, String)>>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            sends: Arc::new(Mutex::new(Vec::new())),
            reactions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn sent_count(&self) -> usize {
        self.sends
            .lock()
            .expect("mutex should not be poisoned")
            .len()
    }

    fn last_body(&self) -> Option<String> {
        self.sends
            .lock()
            .expect("mutex should not be poisoned")
            .last()
            .map(|m| m.body.clone())
    }

    fn reaction_count(&self) -> usize {
        self.reactions
            .lock()
            .expect("mutex should not be poisoned")
            .len()
    }
}

#[async_trait]
impl ChannelSink for RecordingSink {
    async fn send(
        &self,
        _ctx: &crabgent_core::subject::Subject,
        _conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        self.sends
            .lock()
            .expect("mutex should not be poisoned")
            .push(msg.clone());
        Ok(MessageRef::top_level(
            "stub",
            Owner::new("stub:conv"),
            "sink-msg-id",
        ))
    }

    async fn react(
        &self,
        _ctx: &crabgent_core::subject::Subject,
        conv: &Owner,
        parent: &MessageRef,
        emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        self.reactions.lock().expect("test result").push((
            conv.clone(),
            parent.clone(),
            emoji.to_owned(),
        ));
        Ok(MessageRef::top_level(
            "stub",
            conv.clone(),
            parent.id.clone(),
        ))
    }
}

struct FailingProvider;

#[async_trait]
impl Provider for FailingProvider {
    async fn complete(
        &self,
        _req: &crabgent_core::types::LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<crabgent_core::types::LlmResponse, ProviderError> {
        Err(ProviderError::Transport("simulated failure".into()))
    }

    fn name(&self) -> &'static str {
        "failing"
    }

    fn capabilities(&self) -> crabgent_core::provider::ProviderCapabilities {
        crabgent_core::provider::ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("claude-haiku-4-5", "failing")]
    }
}

fn build_failing_kernel() -> Arc<Kernel> {
    Arc::new(
        Kernel::builder()
            .provider(FailingProvider)
            .policy(AllowAllPolicy)
            .build(),
    )
}

/// When the kernel returns Err and live delivery is configured, the sink
/// receives one compact status message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kernel_error_surfaces_via_live_turn_delivery() {
    let sink = Arc::new(RecordingSink::new());
    let inbox = KernelChannelInbox::new(
        build_failing_kernel(),
        "claude-haiku-4-5",
        Arc::new(AllowAllPolicy),
    )
    .with_live_turn_delivery(Arc::clone(&sink) as Arc<dyn ChannelSink>);

    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    inbox.receive(ev).await.expect("receive ok");

    // Wait for the background task to finish.
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        sink.sent_count(),
        2,
        "progress plus final fallback should be sent when edit is unsupported"
    );
    let body = sink.last_body().expect("test result");
    assert_eq!(body, "Processing failed: provider error.");
}

/// Happy path: the final assistant text is delivered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kernel_success_delivers_final() {
    let sink = Arc::new(RecordingSink::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let inbox = KernelChannelInbox::new(
        build_kernel(Arc::clone(&seen)),
        "claude-haiku-4-5",
        Arc::new(AllowAllPolicy),
    )
    .with_live_turn_delivery(Arc::clone(&sink) as Arc<dyn ChannelSink>);

    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    inbox.receive(ev).await.expect("receive ok");

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(sink.sent_count(), 1, "final answer should be sent");
    assert_eq!(sink.last_body().as_deref(), Some("ok"));
}

/// When the lifecycle is already shutting down, no status message is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_does_not_post_live_turn_status() {
    let sink = Arc::new(RecordingSink::new());
    let inbox = KernelChannelInbox::new(
        build_failing_kernel(),
        "claude-haiku-4-5",
        Arc::new(AllowAllPolicy),
    )
    .with_live_turn_delivery(Arc::clone(&sink) as Arc<dyn ChannelSink>);

    // Cancel the lifecycle before receiving so the child token is already fired
    // when run_kernel_with_release checks it.
    inbox.lifecycle.shutdown_with_grace(Duration::ZERO).await;

    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    // receive itself returns ShuttingDown, the kernel is never called.
    let err = inbox.receive(ev).await.expect_err("should be shut down");
    assert!(matches!(err, ChannelError::ShuttingDown));

    // No error send, the error path in run_kernel_with_release was never
    // reached because no task was spawned.
    assert_eq!(sink.sent_count(), 0, "no error send on shutdown");
    assert_eq!(sink.reaction_count(), 0, "no reaction on shutdown");
}

/// Default (no live delivery configured): errors stay silent toward the channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_live_turn_delivery_configured_is_silent() {
    let inbox = KernelChannelInbox::new(
        build_failing_kernel(),
        "claude-haiku-4-5",
        Arc::new(AllowAllPolicy),
    );

    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    inbox.receive(ev).await.expect("receive ok");

    // Wait for the background task to fail silently.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Test passes as long as it did not panic. Nothing to assert on the sink
    // since there is none.
}
