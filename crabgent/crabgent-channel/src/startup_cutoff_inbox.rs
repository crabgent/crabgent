//! Drop inbound events older than a per-process cutoff.
//!
//! Matrix `/sync` (and any other channel adapter without a persisted
//! "since" token) delivers each joined room's recent timeline backlog
//! on initial connect. Without filtering, the bot re-handles every old
//! event it sees during startup, including stale slash-commands that
//! would otherwise re-fire through `CommandDispatchInbox`.
//!
//! `StartupCutoffInbox` is a `ChannelInbox` decorator that records the
//! construction-time wall clock as a cutoff and drops any
//! `InboundEvent` / `InboundReaction` whose `timestamp` predates it.
//! Adapters with persistent offset tracking (Telegram long-poll, Slack
//! Events API replay-with-ack) do not need this decorator; matrix-style
//! initial-sync adapters do.
//!
//! Construct once per agent / poller outside any command-dispatch
//! decorator, so cutoff filtering applies uniformly to plain prompts
//! and to prefix-commands.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_log::debug;

use crate::envelope::{InboundEvent, InboundReaction};
use crate::error::ChannelError;
use crate::inbox::ChannelInbox;

/// `ChannelInbox` decorator that drops events older than process start.
pub struct StartupCutoffInbox {
    cutoff: DateTime<Utc>,
    inner: Arc<dyn ChannelInbox>,
}

impl StartupCutoffInbox {
    /// Build a decorator whose cutoff is the current UTC time.
    #[must_use]
    pub fn new(inner: Arc<dyn ChannelInbox>) -> Self {
        Self {
            cutoff: Utc::now(),
            inner,
        }
    }

    /// Borrow the effective cutoff (test + observability hook).
    #[must_use]
    pub const fn cutoff(&self) -> DateTime<Utc> {
        self.cutoff
    }
}

#[async_trait]
impl ChannelInbox for StartupCutoffInbox {
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        if event.timestamp < self.cutoff {
            debug!(
                channel = %event.channel,
                conv = %event.conv,
                from = %event.from.id,
                ts = %event.timestamp,
                cutoff = %self.cutoff,
                "dropping pre-startup inbound event"
            );
            return Ok(());
        }
        self.inner.receive(event).await
    }

    async fn receive_reaction(&self, reaction: InboundReaction) -> Result<(), ChannelError> {
        if reaction.timestamp < self.cutoff {
            debug!(
                channel = %reaction.channel,
                conv = %reaction.conv,
                from = %reaction.from.id,
                ts = %reaction.timestamp,
                cutoff = %self.cutoff,
                "dropping pre-startup inbound reaction"
            );
            return Ok(());
        }
        self.inner.receive_reaction(reaction).await
    }

    async fn shutdown(&self, grace: std::time::Duration) {
        self.inner.shutdown(grace).await;
    }

    fn blocks_outer_command_dispatch(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::Duration;
    use crabgent_core::Owner;

    use super::*;
    use crate::envelope::MessageRef;
    use crate::participant::{Participant, ParticipantRole};

    #[derive(Default)]
    struct RecordingInbox {
        events: Mutex<Vec<InboundEvent>>,
        reactions: Mutex<Vec<InboundReaction>>,
    }

    #[async_trait]
    impl ChannelInbox for RecordingInbox {
        async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
            self.events
                .lock()
                .expect("test mutex must not be poisoned")
                .push(event);
            Ok(())
        }

        async fn receive_reaction(&self, reaction: InboundReaction) -> Result<(), ChannelError> {
            self.reactions
                .lock()
                .expect("test mutex must not be poisoned")
                .push(reaction);
            Ok(())
        }
    }

    fn event_at(ts: DateTime<Utc>) -> InboundEvent {
        let conv = Owner::new("test:conv");
        InboundEvent {
            channel: "test".to_owned(),
            conv: conv.clone(),
            kind: None,
            from: Participant::new("u1", ParticipantRole::Human),
            message: MessageRef::top_level("test", conv, "in1"),
            body: "hi".to_owned(),
            attachments: Vec::new(),
            timestamp: ts,
        }
    }

    fn reaction_at(ts: DateTime<Utc>) -> InboundReaction {
        let conv = Owner::new("test:conv");
        InboundReaction {
            channel: "test".to_owned(),
            conv: conv.clone(),
            from: Participant::new("u1", ParticipantRole::Human),
            parent: MessageRef::top_level("test", conv, "tgt1"),
            emoji: "+1".to_owned(),
            added: true,
            timestamp: ts,
        }
    }

    #[tokio::test]
    async fn drops_event_older_than_cutoff() {
        let inner = Arc::new(RecordingInbox::default());
        let inbox = StartupCutoffInbox::new(Arc::clone(&inner) as Arc<dyn ChannelInbox>);
        let stale = inbox.cutoff() - Duration::seconds(60);
        inbox.receive(event_at(stale)).await.expect("drop ok");
        assert!(
            inner
                .events
                .lock()
                .expect("test mutex must not be poisoned")
                .is_empty(),
            "stale event must not reach inner inbox",
        );
    }

    #[tokio::test]
    async fn forwards_event_at_or_after_cutoff() {
        let inner = Arc::new(RecordingInbox::default());
        let inbox = StartupCutoffInbox::new(Arc::clone(&inner) as Arc<dyn ChannelInbox>);
        let fresh = inbox.cutoff() + Duration::seconds(1);
        inbox.receive(event_at(fresh)).await.expect("forward ok");
        assert_eq!(
            inner
                .events
                .lock()
                .expect("test mutex must not be poisoned")
                .len(),
            1,
            "fresh event must reach inner inbox",
        );
    }

    #[tokio::test]
    async fn drops_reaction_older_than_cutoff() {
        let inner = Arc::new(RecordingInbox::default());
        let inbox = StartupCutoffInbox::new(Arc::clone(&inner) as Arc<dyn ChannelInbox>);
        let stale = inbox.cutoff() - Duration::seconds(60);
        inbox
            .receive_reaction(reaction_at(stale))
            .await
            .expect("drop ok");
        assert!(
            inner
                .reactions
                .lock()
                .expect("test mutex must not be poisoned")
                .is_empty(),
            "stale reaction must not reach inner inbox",
        );
    }

    #[tokio::test]
    async fn forwards_reaction_at_or_after_cutoff() {
        let inner = Arc::new(RecordingInbox::default());
        let inbox = StartupCutoffInbox::new(Arc::clone(&inner) as Arc<dyn ChannelInbox>);
        let fresh = inbox.cutoff() + Duration::seconds(1);
        inbox
            .receive_reaction(reaction_at(fresh))
            .await
            .expect("forward ok");
        assert_eq!(
            inner
                .reactions
                .lock()
                .expect("test mutex must not be poisoned")
                .len(),
            1,
            "fresh reaction must reach inner inbox",
        );
    }
}
