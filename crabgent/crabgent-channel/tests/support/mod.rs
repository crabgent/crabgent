#![allow(
    dead_code,
    reason = "integration-test support is shared across tests with disjoint helper needs"
)]

use crabgent_core::RunCtx;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_channel::{
    Channel, ChannelKind, MessageRef, OutboundMessage, Participant, ParticipantRole,
};
use crabgent_core::error::ProviderError;
use crabgent_core::owner::Owner;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::subject::Subject;
use crabgent_core::types::{LlmRequest, LlmResponse, StopReason, Usage};
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

pub struct RecordingChannel {
    sends: AtomicUsize,
    lists: AtomicUsize,
}

impl RecordingChannel {
    pub const fn new() -> Self {
        Self {
            sends: AtomicUsize::new(0),
            lists: AtomicUsize::new(0),
        }
    }

    pub fn send_count(&self) -> usize {
        self.sends.load(Ordering::Relaxed)
    }

    pub fn list_count(&self) -> usize {
        self.lists.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl Channel for RecordingChannel {
    fn name(&self) -> &'static str {
        "stub"
    }

    async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, crabgent_channel::ChannelError> {
        Ok(ChannelKind::Group)
    }

    async fn participants(
        &self,
        _ctx: &Subject,
        _conv: &Owner,
    ) -> Result<Vec<Participant>, crabgent_channel::ChannelError> {
        self.lists.fetch_add(1, Ordering::Relaxed);
        Ok(vec![Participant::new("U1", ParticipantRole::Human)])
    }

    async fn send(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        _msg: &OutboundMessage,
    ) -> Result<MessageRef, crabgent_channel::ChannelError> {
        self.sends.fetch_add(1, Ordering::Relaxed);
        Ok(MessageRef::top_level("stub", conv.clone(), "ts:1"))
    }
}

#[derive(Clone)]
pub struct BlockingProvider {
    state: Arc<BlockingState>,
}

struct BlockingState {
    started: AtomicUsize,
    completed: AtomicUsize,
    cancelled: AtomicUsize,
    notify: Notify,
    release: Arc<Semaphore>,
    hold_cancelled: bool,
    cancel_release: Arc<Semaphore>,
}

impl BlockingProvider {
    pub fn new() -> Self {
        Self {
            state: Arc::new(BlockingState {
                started: AtomicUsize::new(0),
                completed: AtomicUsize::new(0),
                cancelled: AtomicUsize::new(0),
                notify: Notify::new(),
                release: Arc::new(Semaphore::new(0)),
                hold_cancelled: false,
                cancel_release: Arc::new(Semaphore::new(0)),
            }),
        }
    }

    pub fn holding_cancelled() -> Self {
        Self {
            state: Arc::new(BlockingState {
                started: AtomicUsize::new(0),
                completed: AtomicUsize::new(0),
                cancelled: AtomicUsize::new(0),
                notify: Notify::new(),
                release: Arc::new(Semaphore::new(0)),
                hold_cancelled: true,
                cancel_release: Arc::new(Semaphore::new(0)),
            }),
        }
    }

    pub fn release(&self, permits: usize) {
        self.state.release.add_permits(permits);
    }

    pub fn release_cancelled(&self, permits: usize) {
        self.state.cancel_release.add_permits(permits);
    }

    pub fn started(&self) -> usize {
        self.state.started.load(Ordering::SeqCst)
    }

    pub fn completed(&self) -> usize {
        self.state.completed.load(Ordering::SeqCst)
    }

    pub fn cancelled(&self) -> usize {
        self.state.cancelled.load(Ordering::SeqCst)
    }

    pub async fn wait_started(&self, expected: usize) {
        self.wait_for(
            || self.started() >= expected,
            "provider started expected runs",
        )
        .await;
    }

    pub async fn wait_completed(&self, expected: usize) {
        self.wait_for(
            || self.completed() >= expected,
            "provider completed expected runs",
        )
        .await;
    }

    pub async fn wait_cancelled(&self, expected: usize) {
        self.wait_for(
            || self.cancelled() >= expected,
            "provider observed expected cancellations",
        )
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

struct BlockingRunGuard {
    state: Arc<BlockingState>,
    counted: bool,
}

impl BlockingRunGuard {
    const fn new(state: Arc<BlockingState>) -> Self {
        Self {
            state,
            counted: false,
        }
    }

    fn complete(&mut self) {
        self.counted = true;
        self.state.completed.fetch_add(1, Ordering::SeqCst);
        self.state.notify.notify_waiters();
    }

    fn cancel(&mut self) {
        self.counted = true;
        self.state.cancelled.fetch_add(1, Ordering::SeqCst);
        self.state.notify.notify_waiters();
    }
}

impl Drop for BlockingRunGuard {
    fn drop(&mut self) {
        if !self.counted {
            self.state.cancelled.fetch_add(1, Ordering::SeqCst);
            self.state.notify.notify_waiters();
        }
    }
}

#[async_trait]
impl Provider for BlockingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.state.started.fetch_add(1, Ordering::SeqCst);
        self.state.notify.notify_waiters();
        let mut guard = BlockingRunGuard::new(Arc::clone(&self.state));
        let release = Arc::clone(&self.state.release).acquire_owned();
        if let Some(cancel) = cancel {
            tokio::select! {
                permit = release => {
                    let _permit = permit.expect("release semaphore open");
                    guard.complete();
                    Ok(blocking_response(req))
                }
                () = cancel.cancelled() => {
                    guard.cancel();
                    if self.state.hold_cancelled {
                        let _permit = Arc::clone(&self.state.cancel_release)
                            .acquire_owned()
                            .await
                            .expect("cancel-release semaphore open");
                    }
                    Err(ProviderError::Cancelled)
                }
            }
        } else {
            let _permit = release.await.expect("release semaphore open");
            guard.complete();
            Ok(blocking_response(req))
        }
    }

    fn name(&self) -> &'static str {
        "blocking"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<crabgent_core::ModelInfo> {
        vec![crabgent_core::ModelInfo::minimal(
            "claude-haiku-4-5",
            "blocking",
        )]
    }
}

fn blocking_response(req: &LlmRequest) -> LlmResponse {
    LlmResponse {
        text: "ok".into(),
        tool_calls: vec![],
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        model: req.model.clone(),
    }
}
