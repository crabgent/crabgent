//! `PairingInbox`: `ChannelInbox` decorator that gates dispatch
//! behind a `/pair <token>` handshake.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::subject::{InvalidSubjectError, Subject};
use crabgent_log::{instrument, warn};
use subtle::ConstantTimeEq;

use super::store::PairingStore;
use crate::channel::ChannelKind;
use crate::envelope::{InboundEvent, MessageRef, OutboundMessage};
use crate::error::ChannelError;
use crate::inbox::ChannelInbox;
use crate::participant::ParticipantRole;
use crate::sink::ChannelSink;
use crate::subject::ChannelSubjectExt;

/// `ChannelInbox` decorator that gates dispatch behind a pairing
/// handshake.
///
/// Behaviour for an inbound `event`:
/// 1. If the body starts with `pair_command_prefix` (default
///    `/pair`): verify the token, add `event.from.id` on success,
///    send a reply via the sink, and return without forwarding to
///    the inner inbox.
/// 2. Else if `event.from.id` is paired: forward to the inner
///    inbox.
/// 3. Else: send the not-paired reply via the sink and drop the
///    event.
///
/// Replies use a synthetic `Subject` with the channel/conv attrs
/// stamped from the event so policies can match on them.
pub struct PairingInbox {
    store: Arc<dyn PairingStore>,
    inner: Arc<dyn ChannelInbox>,
    sink: Arc<dyn ChannelSink>,
    pair_token: String,
    pair_command_prefix: String,
    not_paired_message: String,
    paired_message: String,
    bad_token_message: String,
    inferred_kind: ChannelKind,
}

impl PairingInbox {
    /// Build a pairing decorator. `pair_token` is the secret a user
    /// must supply via `/pair <token>`.
    pub fn new(
        store: Arc<dyn PairingStore>,
        inner: Arc<dyn ChannelInbox>,
        sink: Arc<dyn ChannelSink>,
        pair_token: impl Into<String>,
    ) -> Self {
        Self {
            store,
            inner,
            sink,
            pair_token: pair_token.into(),
            pair_command_prefix: "/pair".into(),
            not_paired_message: "Not paired. Send /pair <token> to pair.".into(),
            paired_message: "Successfully paired.".into(),
            bad_token_message: "Invalid pair token.".into(),
            inferred_kind: ChannelKind::Direct,
        }
    }

    /// Override default reply messages.
    #[must_use]
    pub fn with_messages(
        mut self,
        not_paired: impl Into<String>,
        paired: impl Into<String>,
        bad_token: impl Into<String>,
    ) -> Self {
        self.not_paired_message = not_paired.into();
        self.paired_message = paired.into();
        self.bad_token_message = bad_token.into();
        self
    }

    /// Override the command prefix (default `/pair`).
    #[must_use]
    pub fn with_command_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.pair_command_prefix = prefix.into();
        self
    }

    /// Override the inferred channel kind for synthesized reply
    /// subjects (default `Direct`).
    #[must_use]
    pub const fn with_inferred_kind(mut self, kind: ChannelKind) -> Self {
        self.inferred_kind = kind;
        self
    }

    fn extract_token<'a>(&self, body: &'a str) -> Option<&'a str> {
        body.strip_prefix(&self.pair_command_prefix)
            .map(str::trim_start)
            .map(|rest| rest.split_whitespace().next().unwrap_or(""))
    }

    fn build_reply_subject(&self, event: &InboundEvent) -> Result<Subject, InvalidSubjectError> {
        Ok(Subject::try_new(format!("{}:bot", event.channel))?
            .with_channel(&event.channel, &event.conv, self.inferred_kind)
            .with_participant_role(ParticipantRole::Bot.as_str()))
    }

    async fn send_reply(&self, event: &InboundEvent, body: &str) -> Result<(), ChannelError> {
        let subject = self.build_reply_subject(event)?;
        let mut msg = OutboundMessage::new(body);
        msg = msg.with_metadata("channel", &event.channel);
        if let Some(parent) = thread_anchor(event) {
            msg = msg.in_thread(parent);
        }
        self.sink
            .send(&subject, &event.conv, &msg)
            .await
            .map(|_| ())
    }

    async fn handle_pair_command(
        &self,
        event: &InboundEvent,
        user_id: &str,
        token: &str,
    ) -> Result<(), ChannelError> {
        if constant_time_eq(token.as_bytes(), self.pair_token.as_bytes()) {
            self.handle_valid_pair(event, user_id).await?;
        } else {
            self.send_reply_or_warn(event, &self.bad_token_message, "bad-token reply failed")
                .await;
        }
        Ok(())
    }

    async fn handle_valid_pair(
        &self,
        event: &InboundEvent,
        user_id: &str,
    ) -> Result<(), ChannelError> {
        self.store.add(user_id).await?;
        self.send_reply_or_warn(event, &self.paired_message, "paired reply failed")
            .await;
        Ok(())
    }

    async fn send_reply_or_warn(&self, event: &InboundEvent, body: &str, log_msg: &str) {
        if let Err(err) = self.send_reply(event, body).await {
            warn!(channel = %event.channel, "{log_msg}: {err}");
        }
    }
}

/// Compare two byte slices without an early-exit on the first mismatch.
///
/// The supplied token is attacker-controlled, so a naive `==` leaks the
/// shared-prefix length through timing. This delegates to the vetted `subtle`
/// crate (also used by `crabgent-mcp-server` bearer auth) so the comparison is
/// not collapsed back into an early-exit by the optimizer. Channel round-trip
/// latency dwarfs the residual signal, so this is defence-in-depth rather than
/// a hard requirement.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    bool::from(a.ct_eq(b))
}

fn thread_anchor(event: &InboundEvent) -> Option<MessageRef> {
    if event.message.is_thread_reply() {
        Some(event.message.clone())
    } else {
        None
    }
}

#[async_trait]
impl ChannelInbox for PairingInbox {
    #[instrument(level = "debug", skip(self, event), fields(channel = %event.channel))]
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        let user_id = event.from.id.as_str().to_owned();
        if let Some(token) = self.extract_token(&event.body) {
            let token_owned = token.to_owned();
            return self
                .handle_pair_command(&event, &user_id, &token_owned)
                .await;
        }
        if self.store.is_paired(&user_id).await? {
            return self.inner.receive(event).await;
        }
        if let Err(err) = self.send_reply(&event, &self.not_paired_message).await {
            warn!(channel = %event.channel, "pairing reply failed: {err}");
        }
        Ok(())
    }

    #[instrument(level = "debug", skip(self, reaction), fields(channel = %reaction.channel, added = reaction.added))]
    async fn receive_reaction(
        &self,
        reaction: crate::envelope::InboundReaction,
    ) -> Result<(), ChannelError> {
        // Reactions from unpaired users are silently dropped, mirroring
        // the text-message policy: unpaired senders never reach the
        // inner kernel inbox.
        let user_id = reaction.from.id.as_str().to_owned();
        if self.store.is_paired(&user_id).await? {
            return self.inner.receive_reaction(reaction).await;
        }
        Ok(())
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
    use super::*;
    use crate::channel::Channel;
    use crate::participant::{Participant, ParticipantId};
    use crate::sink::ChannelRouter;
    use chrono::Utc;
    use crabgent_core::owner::Owner;
    use std::sync::Mutex as StdMutex;

    fn build_event(channel: &str, body: &str, user_id: &str) -> InboundEvent {
        InboundEvent {
            channel: channel.to_owned(),
            conv: Owner::new(format!("{channel}:dm-{user_id}")),
            kind: None,
            from: Participant::new(ParticipantId::new(user_id), ParticipantRole::Human),
            message: MessageRef::top_level(
                channel,
                Owner::new(format!("{channel}:dm-{user_id}")),
                "1",
            ),
            body: body.to_owned(),
            attachments: vec![],
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn constant_time_eq_matches_value_equality() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        // Differing lengths never compare equal, including prefix matches.
        assert!(!constant_time_eq(b"secret", b"secret-extra"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }

    struct RecordInbox {
        events: StdMutex<Vec<InboundEvent>>,
    }

    impl RecordInbox {
        const fn new() -> Self {
            Self {
                events: StdMutex::new(Vec::new()),
            }
        }
        fn count(&self) -> usize {
            self.events
                .lock()
                .expect("mutex should not be poisoned")
                .len()
        }
    }

    #[async_trait]
    impl ChannelInbox for RecordInbox {
        async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
            self.events
                .lock()
                .expect("mutex should not be poisoned")
                .push(event);
            Ok(())
        }
    }

    struct CapturingChannel {
        sent: StdMutex<Vec<(Owner, OutboundMessage)>>,
    }

    impl CapturingChannel {
        const fn new() -> Self {
            Self {
                sent: StdMutex::new(Vec::new()),
            }
        }
        fn last_body(&self) -> Option<String> {
            self.sent
                .lock()
                .expect("test result")
                .last()
                .map(|(_, m)| m.body.clone())
        }
        fn count(&self) -> usize {
            self.sent
                .lock()
                .expect("mutex should not be poisoned")
                .len()
        }
    }

    #[async_trait]
    impl Channel for CapturingChannel {
        fn name(&self) -> &'static str {
            "test"
        }
        async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, ChannelError> {
            Ok(ChannelKind::Direct)
        }
        async fn participants(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
        ) -> Result<Vec<Participant>, ChannelError> {
            Ok(vec![])
        }
        async fn send(
            &self,
            _ctx: &Subject,
            conv: &Owner,
            msg: &OutboundMessage,
        ) -> Result<MessageRef, ChannelError> {
            self.sent
                .lock()
                .expect("mutex should not be poisoned")
                .push((conv.clone(), msg.clone()));
            Ok(MessageRef::top_level("test", conv.clone(), "id"))
        }
    }

    fn build_setup() -> (
        Arc<super::super::MemoryPairingStore>,
        Arc<RecordInbox>,
        Arc<CapturingChannel>,
        PairingInbox,
    ) {
        let store = Arc::new(super::super::MemoryPairingStore::new());
        let inner = Arc::new(RecordInbox::new());
        let channel = Arc::new(CapturingChannel::new());
        let trait_obj: Arc<dyn Channel> = Arc::clone(&channel) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        let inbox = PairingInbox::new(
            Arc::clone(&store) as Arc<dyn PairingStore>,
            Arc::clone(&inner) as Arc<dyn ChannelInbox>,
            router,
            "secret",
        );
        (store, inner, channel, inbox)
    }

    #[tokio::test]
    async fn pair_command_with_correct_token_pairs_user() {
        let (store, inner, channel, inbox) = build_setup();
        let ev = build_event("test", "/pair secret", "U1");
        inbox.receive(ev).await.expect("test result");
        assert!(store.is_paired("U1").await.expect("test result"));
        assert_eq!(inner.count(), 0);
        assert_eq!(channel.count(), 1);
        assert_eq!(
            channel.last_body().expect("test result"),
            "Successfully paired."
        );
    }

    #[tokio::test]
    async fn pair_command_with_wrong_token_does_not_pair() {
        let (store, inner, channel, inbox) = build_setup();
        let ev = build_event("test", "/pair wrong", "U1");
        inbox.receive(ev).await.expect("test result");
        assert!(!store.is_paired("U1").await.expect("test result"));
        assert_eq!(inner.count(), 0);
        assert_eq!(
            channel.last_body().expect("test result"),
            "Invalid pair token."
        );
    }

    #[tokio::test]
    async fn unpaired_user_message_replies_without_forwarding() {
        let (_store, inner, channel, inbox) = build_setup();
        let ev = build_event("test", "hello", "U2");
        inbox.receive(ev).await.expect("test result");
        assert_eq!(inner.count(), 0);
        assert_eq!(channel.count(), 1);
        let body = channel.last_body().expect("test result");
        assert!(body.contains("Not paired"));
    }

    #[tokio::test]
    async fn paired_user_message_forwards_to_inner() {
        let (store, inner, channel, inbox) = build_setup();
        store.add("U1").await.expect("test result");
        let ev = build_event("test", "hello", "U1");
        inbox.receive(ev).await.expect("test result");
        assert_eq!(inner.count(), 1);
        assert_eq!(channel.count(), 0);
    }

    #[tokio::test]
    async fn with_messages_overrides_replies() {
        let store = Arc::new(super::super::MemoryPairingStore::new());
        let inner = Arc::new(RecordInbox::new());
        let channel = Arc::new(CapturingChannel::new());
        let trait_obj: Arc<dyn Channel> = Arc::clone(&channel) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        let inbox = PairingInbox::new(
            Arc::clone(&store) as Arc<dyn PairingStore>,
            Arc::clone(&inner) as Arc<dyn ChannelInbox>,
            router,
            "secret",
        )
        .with_messages("not paired DE", "paired DE", "bad token DE");
        let ev = build_event("test", "hi", "U1");
        inbox.receive(ev).await.expect("test result");
        assert_eq!(channel.last_body().expect("test result"), "not paired DE");
    }

    #[tokio::test]
    async fn with_command_prefix_changes_pair_trigger() {
        let store = Arc::new(super::super::MemoryPairingStore::new());
        let inner = Arc::new(RecordInbox::new());
        let channel = Arc::new(CapturingChannel::new());
        let trait_obj: Arc<dyn Channel> = Arc::clone(&channel) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        let inbox = PairingInbox::new(
            Arc::clone(&store) as Arc<dyn PairingStore>,
            Arc::clone(&inner) as Arc<dyn ChannelInbox>,
            router,
            "secret",
        )
        .with_command_prefix("/connect");
        let ev = build_event("test", "/connect secret", "U1");
        inbox.receive(ev).await.expect("test result");
        assert!(store.is_paired("U1").await.expect("test result"));
    }

    #[tokio::test]
    async fn with_inferred_kind_overrides_default() {
        let store = Arc::new(super::super::MemoryPairingStore::new());
        let inner = Arc::new(RecordInbox::new());
        let channel = Arc::new(CapturingChannel::new());
        let trait_obj: Arc<dyn Channel> = Arc::clone(&channel) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        let inbox = PairingInbox::new(
            Arc::clone(&store) as Arc<dyn PairingStore>,
            Arc::clone(&inner) as Arc<dyn ChannelInbox>,
            router,
            "secret",
        )
        .with_inferred_kind(ChannelKind::Group);
        let ev = build_event("test", "/pair secret", "U1");
        inbox.receive(ev).await.expect("test result");
    }

    #[tokio::test]
    async fn thread_reply_preserves_thread_anchor_in_reply() {
        let store = Arc::new(super::super::MemoryPairingStore::new());
        let inner = Arc::new(RecordInbox::new());
        let channel = Arc::new(CapturingChannel::new());
        let trait_obj: Arc<dyn Channel> = Arc::clone(&channel) as _;
        let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(trait_obj));
        let inbox = PairingInbox::new(
            Arc::clone(&store) as Arc<dyn PairingStore>,
            Arc::clone(&inner) as Arc<dyn ChannelInbox>,
            router,
            "secret",
        );
        let mut ev = build_event("test", "hello", "U1");
        ev.message = MessageRef::thread_reply("test", Owner::new("test:dm-U1"), "1", "root-99");
        inbox.receive(ev).await.expect("test result");
        let last = channel.sent.lock().expect("mutex should not be poisoned");
        let (_, msg) = last.last().expect("last item should exist");
        assert!(msg.thread_parent.is_some());
        assert_eq!(
            msg.thread_parent
                .as_ref()
                .expect("test result")
                .thread_root
                .as_deref(),
            Some("root-99")
        );
    }
}
