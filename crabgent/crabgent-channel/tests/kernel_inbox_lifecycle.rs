use crabgent_core::RunCtx;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::{
    ChannelError, ChannelInbox, InboundEvent, KernelChannelInbox, MessageRef, Participant,
    ParticipantRole,
};
use crabgent_core::Kernel;
use crabgent_core::error::ProviderError;
use crabgent_core::owner::Owner;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

mod support;
use support::BlockingProvider;

#[derive(Clone)]
struct IgnoringCancelProvider {
    state: Arc<IgnoringCancelState>,
}

struct IgnoringCancelState {
    active: AtomicUsize,
    dropped: AtomicUsize,
    notify: Notify,
}

impl IgnoringCancelProvider {
    fn new() -> Self {
        Self {
            state: Arc::new(IgnoringCancelState {
                active: AtomicUsize::new(0),
                dropped: AtomicUsize::new(0),
                notify: Notify::new(),
            }),
        }
    }

    fn active(&self) -> usize {
        self.state.active.load(Ordering::SeqCst)
    }

    fn dropped(&self) -> usize {
        self.state.dropped.load(Ordering::SeqCst)
    }

    async fn wait_active(&self, expected: usize) {
        self.wait_for(
            || self.active() >= expected,
            "provider stream became active",
        )
        .await;
    }

    async fn wait_dropped(&self, expected: usize) {
        self.wait_for(|| self.dropped() >= expected, "provider stream was dropped")
            .await;
    }

    async fn wait_for(&self, predicate: impl Fn() -> bool + Send + Sync, msg: &'static str) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !predicate() {
                self.state.notify.notified().await;
            }
        })
        .await
        .expect(msg);
    }
}

#[async_trait]
impl Provider for IgnoringCancelProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.state.active.fetch_add(1, Ordering::SeqCst);
        self.state.notify.notify_waiters();
        let _guard = IgnoringCancelGuard {
            state: Arc::clone(&self.state),
        };
        std::future::pending::<Result<LlmResponse, ProviderError>>().await
    }

    fn name(&self) -> &'static str {
        "ignoring-cancel"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<crabgent_core::ModelInfo> {
        vec![crabgent_core::ModelInfo::minimal(
            "claude-haiku-4-5",
            "ignoring-cancel",
        )]
    }
}

struct IgnoringCancelGuard {
    state: Arc<IgnoringCancelState>,
}

impl Drop for IgnoringCancelGuard {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::SeqCst);
        self.state.dropped.fetch_add(1, Ordering::SeqCst);
        self.state.notify.notify_waiters();
    }
}

fn blocking_inbox(provider: BlockingProvider, max_concurrent: usize) -> Arc<KernelChannelInbox> {
    let kernel = Arc::new(
        Kernel::builder()
            .provider(provider)
            .policy(AllowAllPolicy)
            .build(),
    );
    Arc::new(
        KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
            .with_max_concurrent_runs(max_concurrent),
    )
}

fn ignoring_cancel_inbox(provider: IgnoringCancelProvider) -> Arc<KernelChannelInbox> {
    let kernel = Arc::new(
        Kernel::builder()
            .provider(provider)
            .policy(AllowAllPolicy)
            .build(),
    );
    Arc::new(
        KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
            .with_max_concurrent_runs(1)
            .with_shutdown_grace(Duration::from_millis(50)),
    )
}

fn build_event(body: &str) -> InboundEvent {
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

/// Build an event whose conv is unique per `conv_id`. Use this in tests that
/// need to spawn independent parallel runs (not inject into an existing one).
fn build_event_unique_conv(body: &str, conv_id: &str) -> InboundEvent {
    let conv = format!("slack:T1/{conv_id}");
    InboundEvent {
        channel: "slack".to_owned(),
        conv: Owner::new(&conv),
        kind: None,
        from: Participant::new("U1", ParticipantRole::Human),
        message: MessageRef::top_level("slack", Owner::new(&conv), "ts:1"),
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
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("in-flight count reached expected value");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_drains_in_flight_runs() {
    let provider = BlockingProvider::new();
    let inbox = blocking_inbox(provider.clone(), 3);
    for idx in 0..3 {
        inbox
            .receive(build_event_unique_conv(&idx.to_string(), &idx.to_string()))
            .await
            .expect("receive ok");
    }

    provider.wait_started(3).await;
    assert_eq!(inbox.in_flight_runs().await, 3);

    inbox.shutdown(Duration::ZERO).await;

    provider.wait_cancelled(3).await;
    wait_in_flight(&inbox, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_drops_inner_driver_when_provider_ignores_cancel() {
    let provider = IgnoringCancelProvider::new();
    let inbox = ignoring_cancel_inbox(provider.clone());
    inbox
        .receive(build_event("hanging stream"))
        .await
        .expect("receive ok");
    provider.wait_active(1).await;
    assert_eq!(inbox.in_flight_runs().await, 1);

    inbox.shutdown(Duration::ZERO).await;

    provider.wait_dropped(1).await;
    assert_eq!(provider.active(), 0);
    wait_in_flight(&inbox, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backpressure_blocks_when_max_reached() {
    let provider = BlockingProvider::new();
    let inbox = blocking_inbox(provider.clone(), 2);
    inbox
        .receive(build_event_unique_conv("one", "D1"))
        .await
        .expect("first receive ok");
    inbox
        .receive(build_event_unique_conv("two", "D2"))
        .await
        .expect("second receive ok");
    provider.wait_started(2).await;

    let third_inbox = Arc::clone(&inbox);
    let mut third = tokio::spawn(async move {
        third_inbox
            .receive(build_event_unique_conv("three", "D3"))
            .await
    });

    tokio::time::timeout(Duration::from_millis(80), &mut third)
        .await
        .expect_err("expected error");

    provider.release(1);
    tokio::time::timeout(Duration::from_secs(1), &mut third)
        .await
        .expect("third receive unblocked")
        .expect("third task joined")
        .expect("third receive ok");
    provider.wait_started(3).await;

    provider.release(2);
    provider.wait_completed(3).await;
    wait_in_flight(&inbox, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receive_after_shutdown_returns_err() {
    let provider = BlockingProvider::new();
    let inbox = blocking_inbox(provider, 1);
    inbox.shutdown(Duration::ZERO).await;
    let err = inbox
        .receive(build_event("late"))
        .await
        .expect_err("shutting down");
    assert!(matches!(err, ChannelError::ShuttingDown));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_flight_counter_tracks_completion() {
    let provider = BlockingProvider::new();
    let inbox = blocking_inbox(provider.clone(), 1);
    inbox.receive(build_event("one")).await.expect("receive ok");
    provider.wait_started(1).await;
    assert_eq!(inbox.in_flight_runs().await, 1);

    provider.release(1);
    provider.wait_completed(1).await;
    wait_in_flight(&inbox, 0).await;
}
