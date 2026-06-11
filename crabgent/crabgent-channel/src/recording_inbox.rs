//! Inbound recording decorator.
//!
//! `RecordingInbox` runs before `KernelChannelInbox` and therefore
//! before its `PolicyHook` check. Recorder implementations own their
//! policy gating and trust-boundary checks.
//!
//! Producer-side validation already applied to `InboundEvent` by the
//! adapter before it reaches `RecordingInbox`:
//! - body length capped at `INBOUND_BODY_MAX_BYTES`
//!   (`crabgent-channel::inbox::hint`) and unicode-sanitised by
//!   `sanitize_for_prompt` against the `General_Category` allowlist
//!   (L/N/P/S/Zs) before XML escaping;
//! - image attachments validated against `ImageValidator`
//!   (`IMAGE_PAYLOAD_MAX_BYTES`, magic-byte sniff, MIME whitelist)
//!   so `ImagePayload` bytes and MIME already passed
//!   size + format gates;
//! - audio attachments validated against `AudioValidator`
//!   (`AUDIO_PAYLOAD_MAX_BYTES`, MIME whitelist);
//! - `MessageRef` carries adapter-verified `source`+`id`, never
//!   adapter-unsanitised user input;
//! - `Participant` carries adapter-attested role
//!   (`Human`/`Bot`/`Agent`).
//!
//! Recorders may therefore treat `InboundEvent` as size- and
//! shape-validated structurally, but the body text and attachment
//! bytes still come from an untrusted producer; do not treat them as
//! safe instructions or de-quote them before storage or LLM
//! inclusion.

use std::sync::Arc;

use async_trait::async_trait;

use crate::envelope::InboundEvent;
use crate::error::ChannelError;
use crate::inbox::ChannelInbox;

/// What to do with an inbound event after recording.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RecordDecision {
    /// Forward the event to the wrapped inbox.
    Forward,
    /// Drop the event after recording.
    #[default]
    Stop,
}

/// Records an inbound event before it reaches the wrapped inbox.
#[async_trait]
pub trait InboundRecorder: Send + Sync {
    async fn record(&self, event: &InboundEvent) -> Result<RecordDecision, ChannelError>;
}

/// `ChannelInbox` decorator that records inbound events before dispatch.
pub struct RecordingInbox<I: ChannelInbox> {
    recorder: Arc<dyn InboundRecorder>,
    next: I,
}

impl<I: ChannelInbox> RecordingInbox<I> {
    #[must_use]
    pub fn new(recorder: Arc<dyn InboundRecorder>, next: I) -> Self {
        Self { recorder, next }
    }
}

#[async_trait]
impl<I: ChannelInbox> ChannelInbox for RecordingInbox<I> {
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        match self.recorder.record(&event).await? {
            RecordDecision::Forward => self.next.receive(event).await,
            RecordDecision::Stop => Ok(()),
        }
    }

    crate::forward_channel_inbox_methods!(next);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Utc;
    use crabgent_core::Owner;

    use super::*;
    use crate::envelope::InboundReaction;
    use crate::envelope::MessageRef;
    use crate::participant::{Participant, ParticipantRole};

    #[derive(Clone, Default)]
    struct CountingInbox {
        received: Arc<AtomicUsize>,
        reactions: Arc<AtomicUsize>,
    }

    impl CountingInbox {
        fn received_count(&self) -> usize {
            self.received.load(Ordering::SeqCst)
        }

        fn reaction_count(&self) -> usize {
            self.reactions.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ChannelInbox for CountingInbox {
        async fn receive(&self, _event: InboundEvent) -> Result<(), ChannelError> {
            self.received.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn receive_reaction(&self, _reaction: InboundReaction) -> Result<(), ChannelError> {
            self.reactions.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct StubRecorder {
        decision: Option<RecordDecision>,
        calls: AtomicUsize,
    }

    impl StubRecorder {
        const fn new(decision: RecordDecision) -> Self {
            Self {
                decision: Some(decision),
                calls: AtomicUsize::new(0),
            }
        }

        const fn error() -> Self {
            Self {
                decision: None,
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl InboundRecorder for StubRecorder {
        async fn record(&self, _event: &InboundEvent) -> Result<RecordDecision, ChannelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.decision
                .ok_or_else(|| ChannelError::adapter("record failed"))
        }
    }

    fn conv() -> Owner {
        Owner::new("test:conv")
    }

    fn participant() -> Participant {
        Participant::new("U1", ParticipantRole::Human)
    }

    fn event() -> InboundEvent {
        InboundEvent {
            channel: "test".to_owned(),
            conv: conv(),
            kind: None,
            from: participant(),
            message: MessageRef::top_level("test", conv(), "m1"),
            body: "hello".to_owned(),
            attachments: Vec::new(),
            timestamp: Utc::now(),
        }
    }

    fn reaction() -> InboundReaction {
        InboundReaction {
            channel: "test".to_owned(),
            conv: conv(),
            from: participant(),
            parent: MessageRef::top_level("test", conv(), "m1"),
            emoji: "+1".to_owned(),
            added: true,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn forward_delegates_to_inner() {
        let recorder = Arc::new(StubRecorder::new(RecordDecision::Forward));
        let recorder_dyn: Arc<dyn InboundRecorder> = recorder.clone();
        let inner = CountingInbox::default();
        let inbox = RecordingInbox::new(recorder_dyn, inner.clone());

        inbox.receive(event()).await.expect("test result");

        assert_eq!(recorder.call_count(), 1);
        assert_eq!(inner.received_count(), 1);
    }

    #[tokio::test]
    async fn stop_swallows_event() {
        let recorder = Arc::new(StubRecorder::new(RecordDecision::Stop));
        let recorder_dyn: Arc<dyn InboundRecorder> = recorder.clone();
        let inner = CountingInbox::default();
        let inbox = RecordingInbox::new(recorder_dyn, inner.clone());

        inbox.receive(event()).await.expect("test result");

        assert_eq!(recorder.call_count(), 1);
        assert_eq!(inner.received_count(), 0);
    }

    #[tokio::test]
    async fn record_error_propagates() {
        let recorder = Arc::new(StubRecorder::error());
        let recorder_dyn: Arc<dyn InboundRecorder> = recorder.clone();
        let inner = CountingInbox::default();
        let inbox = RecordingInbox::new(recorder_dyn, inner.clone());

        let result = inbox.receive(event()).await;

        assert!(result.is_err());
        assert_eq!(recorder.call_count(), 1);
        assert_eq!(inner.received_count(), 0);
    }

    #[tokio::test]
    async fn reaction_delegates_transparently() {
        let recorder = Arc::new(StubRecorder::new(RecordDecision::Stop));
        let recorder_dyn: Arc<dyn InboundRecorder> = recorder.clone();
        let inner = CountingInbox::default();
        let inbox = RecordingInbox::new(recorder_dyn, inner.clone());

        inbox
            .receive_reaction(reaction())
            .await
            .expect("test result");

        assert_eq!(recorder.call_count(), 0);
        assert_eq!(inner.received_count(), 0);
        assert_eq!(inner.reaction_count(), 1);
    }
}
