use std::sync::Arc;

use async_trait::async_trait;
use crabgent_channel::{
    ChannelInbox, ChannelKind, ChannelRouter, InboundEvent, MemoryPairingStore, MessageRef,
    PairingInbox, Participant, ParticipantRole, StartupCutoffInbox, attr_keys,
};
use crabgent_command::{CommandAgentName, CommandError, CommandRegistry, SessionStore};
use crabgent_core::{Action, AllowAllPolicy, Owner, PolicyDecision, PolicyHook, Subject};
use crabgent_store::memory::MemorySessionStore;
use crabgent_test_support::{CountingInbox, StubCommand};
use matrix_sdk::{Client, ruma::owned_user_id};
use url::Url;

use super::{DEFAULT_COMMAND_PREFIX, MatrixChannel, MatrixSyncPoller};

fn stub_command() -> Arc<StubCommand> {
    Arc::new(StubCommand::new("stub").without_sink_reply())
}

async fn poller(inner: Arc<dyn ChannelInbox>) -> MatrixSyncPoller {
    let (poller, _channel) = poller_with_channel(inner).await;
    poller
}

async fn poller_with_channel(
    inner: Arc<dyn ChannelInbox>,
) -> (MatrixSyncPoller, Arc<MatrixChannel>) {
    let client = Client::new(Url::parse("https://example.org").expect("test url"))
        .await
        .expect("matrix client");
    let channel = Arc::new(MatrixChannel::from_client(
        client,
        owned_user_id!("@bot:example.org"),
        None,
    ));
    (MatrixSyncPoller::new(Arc::clone(&channel), inner), channel)
}

fn registry(command: Arc<StubCommand>) -> CommandRegistry {
    CommandRegistry::new()
        .with_command(command)
        .expect("registry accepts stub command")
}

fn agent_name() -> CommandAgentName {
    CommandAgentName::parse("nova").expect("valid test agent name")
}

fn event(body: &str) -> InboundEvent {
    let conv = Owner::new("matrix:!room:example.org");
    InboundEvent {
        channel: "matrix".to_owned(),
        conv: conv.clone(),
        kind: Some(ChannelKind::Group),
        from: Participant::new("@alice:example.org", ParticipantRole::Human),
        message: MessageRef::top_level("matrix", conv, "$event"),
        body: body.to_owned(),
        attachments: Vec::new(),
        timestamp: crabgent_store::Utc::now(),
    }
}

struct DirectMatrixPolicy;

#[async_trait]
impl PolicyHook for DirectMatrixPolicy {
    async fn allow(&self, subject: &Subject, _action: &Action) -> PolicyDecision {
        if subject.attr("agent") == Some("nova")
            && subject.attr(attr_keys::CHANNEL_KIND) == Some("direct")
        {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny("missing matrix command subject attrs".to_owned())
        }
    }
}

fn store() -> Arc<dyn SessionStore> {
    Arc::new(MemorySessionStore::default())
}

fn pairing_inbox(inner: Arc<dyn ChannelInbox>) -> Arc<dyn ChannelInbox> {
    Arc::new(PairingInbox::new(
        Arc::new(MemoryPairingStore::new()),
        inner,
        Arc::new(ChannelRouter::new()),
        "pair-token",
    ))
}

fn startup_cutoff_inbox(inner: Arc<dyn ChannelInbox>) -> Arc<dyn ChannelInbox> {
    Arc::new(StartupCutoffInbox::new(inner))
}

#[test]
fn default_command_prefix_is_bang() {
    assert_eq!(DEFAULT_COMMAND_PREFIX, "!");
}

#[tokio::test]
async fn builder_with_commands_wraps_inbox() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();
    let poller = poller(inner as Arc<dyn ChannelInbox>).await.with_commands(
        registry(Arc::clone(&command)),
        agent_name(),
        None,
        store(),
        Arc::new(AllowAllPolicy),
    );

    assert_eq!(poller.command_prefix().expect("commands").as_str(), "!");
}

#[tokio::test]
async fn no_commands_passes_through() {
    let inner = Arc::new(CountingInbox::default());
    let poller = poller(inner.clone() as Arc<dyn ChannelInbox>).await;

    poller
        .dispatch_inbox()
        .receive(event("!stub hi"))
        .await
        .expect("receive");

    assert_eq!(inner.received_count(), 1);
}

#[tokio::test]
async fn prefixed_event_dispatches_via_command_handler() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();
    let poller = poller(inner.clone() as Arc<dyn ChannelInbox>)
        .await
        .with_commands(
            registry(Arc::clone(&command)),
            agent_name(),
            None,
            store(),
            Arc::new(AllowAllPolicy),
        );

    poller
        .dispatch_inbox()
        .receive(event("!stub hi"))
        .await
        .expect("receive");

    assert_eq!(command.calls(), 1);
    assert_eq!(inner.received_count(), 0);
}

#[tokio::test]
async fn commands_reject_pairing_inbox_outer_bypass() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();
    let result = poller(pairing_inbox(inner as Arc<dyn ChannelInbox>))
        .await
        .try_with_commands(
            registry(command),
            agent_name(),
            None,
            store(),
            Arc::new(AllowAllPolicy),
        );

    assert!(matches!(result, Err(CommandError::InvalidComposition)));
}

#[tokio::test]
#[should_panic(
    expected = "invalid command dispatch configuration: command dispatch must be composed inside mandatory channel gates"
)]
async fn with_commands_panics_with_composition_context() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();

    let _poller = poller(pairing_inbox(inner as Arc<dyn ChannelInbox>))
        .await
        .with_commands(
            registry(command),
            agent_name(),
            None,
            store(),
            Arc::new(AllowAllPolicy),
        );
}

#[tokio::test]
async fn commands_reject_startup_cutoff_outer_bypass() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();
    let result = poller(startup_cutoff_inbox(inner as Arc<dyn ChannelInbox>))
        .await
        .try_with_commands(
            registry(command),
            agent_name(),
            None,
            store(),
            Arc::new(AllowAllPolicy),
        );

    assert!(matches!(result, Err(CommandError::InvalidComposition)));
}

#[tokio::test]
async fn command_dispatch_uses_matrix_subject_resolver() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();
    let (poller, channel) = poller_with_channel(inner as Arc<dyn ChannelInbox>).await;
    let room_id = "!room:example.org".try_into().expect("valid test room id");
    channel
        .kind_cache()
        .lock()
        .expect("matrix room-kind cache lock should not be poisoned")
        .insert(room_id, ChannelKind::Direct);
    let poller = poller.with_commands(
        registry(Arc::clone(&command)),
        agent_name(),
        None,
        store(),
        Arc::new(DirectMatrixPolicy),
    );
    let mut ev = event("!stub hi");
    ev.kind = None;

    poller.dispatch_inbox().receive(ev).await.expect("receive");

    assert_eq!(command.calls(), 1);
}
