//! Bullet 1.2 tests: `receive` and `receive_reaction` resolve
//! `Channel::conv_display` once and stamp the readable display attrs onto
//! the dispatched subject. The policy hook is the observable boundary: it
//! sees the fully-stamped `req.subject` before the run is spawned.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use crabgent_core::action::Action;
use crabgent_core::owner::Owner;
use crabgent_core::policy::{PolicyDecision, PolicyHook};
use crabgent_core::subject::Subject;

use super::super::{ChannelInbox, KernelChannelInbox};
use super::build_kernel;
use crate::channel::{Channel, ChannelKind, ConvLabel};
use crate::envelope::{InboundEvent, InboundReaction, MessageRef, OutboundMessage};
use crate::error::ChannelError;
use crate::participant::{Participant, ParticipantRole};
use crate::subject::attr_keys;

/// Channel mock returning a fixed `ConvLabel` from `conv_display` and
/// counting how often it was called (the dispatch path must resolve it
/// exactly once per event).
struct LabeledChannel {
    label: Option<ConvLabel>,
    calls: Arc<Mutex<usize>>,
}

#[async_trait]
impl Channel for LabeledChannel {
    fn name(&self) -> &'static str {
        "slack"
    }

    async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, ChannelError> {
        Ok(ChannelKind::Group)
    }

    async fn conv_display(&self, _conv: &Owner) -> Option<ConvLabel> {
        *self.calls.lock().expect("calls mutex") += 1;
        self.label.clone()
    }

    async fn participants(
        &self,
        _ctx: &Subject,
        _conv: &Owner,
    ) -> Result<Vec<Participant>, ChannelError> {
        Ok(Vec::new())
    }

    async fn send(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        _msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        Ok(MessageRef::top_level("slack", conv.clone(), "id"))
    }
}

/// Policy hook that records the display attrs from each subject it gates.
struct RecordingPolicy {
    seen: Arc<Mutex<Vec<DisplaySnapshot>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DisplaySnapshot {
    channel: Option<String>,
    workspace: Option<String>,
    sender: Option<String>,
}

impl DisplaySnapshot {
    fn capture(subject: &Subject) -> Self {
        Self {
            channel: subject.attr(attr_keys::CHANNEL_DISPLAY).map(str::to_owned),
            workspace: subject
                .attr(attr_keys::WORKSPACE_DISPLAY)
                .map(str::to_owned),
            sender: subject.attr(attr_keys::SENDER_DISPLAY).map(str::to_owned),
        }
    }
}

#[async_trait]
impl PolicyHook for RecordingPolicy {
    async fn allow(&self, subject: &Subject, _action: &Action) -> PolicyDecision {
        self.seen
            .lock()
            .expect("seen mutex")
            .push(DisplaySnapshot::capture(subject));
        PolicyDecision::Allow
    }
}

/// Inbox plus the policy-capture log and the `conv_display` call counter.
type LabeledHarness = (
    KernelChannelInbox,
    Arc<Mutex<Vec<DisplaySnapshot>>>,
    Arc<Mutex<usize>>,
);

fn labeled_inbox(label: Option<ConvLabel>) -> LabeledHarness {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(Mutex::new(0));
    let channel: Arc<dyn Channel> = Arc::new(LabeledChannel {
        label,
        calls: Arc::clone(&calls),
    });
    let inbox = KernelChannelInbox::new(
        build_kernel(Arc::new(Mutex::new(Vec::new()))),
        "claude-haiku-4-5",
        Arc::new(RecordingPolicy {
            seen: Arc::clone(&seen),
        }),
    )
    .with_inferred_kind(ChannelKind::Group)
    .with_conv_display_channel(channel);
    (inbox, seen, calls)
}

fn group_event(sender_display: Option<&str>) -> InboundEvent {
    let mut from = Participant::new("U1", ParticipantRole::Human);
    from.display_name = sender_display.map(str::to_owned);
    InboundEvent {
        channel: "slack".to_owned(),
        conv: Owner::new("slack:T1/C1"),
        kind: None,
        from,
        message: MessageRef::top_level("slack", Owner::new("slack:T1/C1"), "ts:1"),
        body: "hi".to_owned(),
        attachments: vec![],
        timestamp: Utc::now(),
    }
}

fn group_reaction(sender_display: Option<&str>) -> InboundReaction {
    let mut from = Participant::new("U1", ParticipantRole::Human);
    from.display_name = sender_display.map(str::to_owned);
    InboundReaction {
        channel: "slack".to_owned(),
        conv: Owner::new("slack:T1/C1"),
        from,
        parent: MessageRef::top_level("slack", Owner::new("slack:T1/C1"), "ts:42"),
        emoji: "+1".to_owned(),
        added: true,
        timestamp: Utc::now(),
    }
}

fn full_label() -> ConvLabel {
    ConvLabel {
        name: Some("#platform-ops".to_owned()),
        workspace: Some("example".to_owned()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receive_message_stamps_display_attrs() {
    let (inbox, seen, calls) = labeled_inbox(Some(full_label()));
    inbox
        .receive(group_event(Some("Alice")))
        .await
        .expect("receive ok");

    let snaps = seen.lock().expect("seen mutex");
    assert_eq!(snaps.len(), 1, "policy gated exactly once");
    assert_eq!(
        snaps[0],
        DisplaySnapshot {
            channel: Some("#platform-ops".to_owned()),
            workspace: Some("example".to_owned()),
            sender: Some("Alice".to_owned()),
        }
    );
    assert_eq!(*calls.lock().expect("calls mutex"), 1, "conv_display once");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receive_reaction_stamps_display_attrs() {
    let (inbox, seen, calls) = labeled_inbox(Some(full_label()));
    inbox
        .receive_reaction(group_reaction(Some("Bob")))
        .await
        .expect("receive_reaction ok");

    let snaps = seen.lock().expect("seen mutex");
    assert_eq!(snaps.len(), 1, "policy gated exactly once");
    assert_eq!(
        snaps[0],
        DisplaySnapshot {
            channel: Some("#platform-ops".to_owned()),
            workspace: Some("example".to_owned()),
            sender: Some("Bob".to_owned()),
        }
    );
    assert_eq!(*calls.lock().expect("calls mutex"), 1, "conv_display once");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_channel_omits_conv_labels_but_keeps_sender() {
    // No conv_display channel installed: channel/workspace stay absent, the
    // sender display still flows through the sync subject resolver.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let inbox = KernelChannelInbox::new(
        build_kernel(Arc::new(Mutex::new(Vec::new()))),
        "claude-haiku-4-5",
        Arc::new(RecordingPolicy {
            seen: Arc::clone(&seen),
        }),
    )
    .with_inferred_kind(ChannelKind::Group);

    inbox
        .receive(group_event(Some("Carol")))
        .await
        .expect("receive ok");

    let snaps = seen.lock().expect("seen mutex");
    assert_eq!(
        snaps[0],
        DisplaySnapshot {
            channel: None,
            workspace: None,
            sender: Some("Carol".to_owned()),
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conv_display_none_result_omits_labels() {
    // Channel installed but conv_display yields None: no labels stamped.
    let (inbox, seen, calls) = labeled_inbox(None);
    inbox.receive(group_event(None)).await.expect("receive ok");

    let snaps = seen.lock().expect("seen mutex");
    assert_eq!(
        snaps[0],
        DisplaySnapshot {
            channel: None,
            workspace: None,
            sender: None,
        }
    );
    assert_eq!(*calls.lock().expect("calls mutex"), 1);
}
