use std::sync::Arc;

use crabgent_channel::{
    ChannelInbox, ChannelKind, ChannelRouter, InboundEvent, MemoryPairingStore, MessageRef,
    PairingInbox, Participant, ParticipantRole, StartupCutoffInbox,
};
use crabgent_command::{CommandAgentName, CommandError, CommandRegistry, SessionStore};
use crabgent_core::{AllowAllPolicy, Owner};
use crabgent_store::memory::MemorySessionStore;
use crabgent_test_support::{CountingInbox, StubCommand};

use super::commands::DEFAULT_COMMAND_PREFIX;
use super::{TelegramChannel, TelegramPoller};

fn stub_command() -> Arc<StubCommand> {
    Arc::new(StubCommand::new("stub").without_sink_reply())
}

fn poller(inner: Arc<dyn ChannelInbox>) -> TelegramPoller {
    let channel = Arc::new(TelegramChannel::new("tk", "B-1", "crabgent_bot"));
    TelegramPoller::new(channel, inner)
}

fn registry(command: Arc<StubCommand>) -> CommandRegistry {
    CommandRegistry::new()
        .with_command(command)
        .expect("registry accepts stub command")
}

fn agent_name() -> CommandAgentName {
    CommandAgentName::parse("worker").expect("valid test agent name")
}

fn event(body: &str) -> InboundEvent {
    let conv = Owner::new("telegram:42");
    InboundEvent {
        channel: "telegram".to_owned(),
        conv: conv.clone(),
        kind: Some(ChannelKind::Direct),
        from: Participant::new("7", ParticipantRole::Human),
        message: MessageRef::top_level("telegram", conv, "1"),
        body: body.to_owned(),
        attachments: Vec::new(),
        timestamp: crabgent_store::Utc::now(),
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
fn default_command_prefix_is_slash() {
    assert_eq!(DEFAULT_COMMAND_PREFIX, "/");
}

#[test]
fn builder_with_commands_wraps_inbox() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();
    let poller = poller(inner as Arc<dyn ChannelInbox>).with_commands(
        registry(Arc::clone(&command)),
        agent_name(),
        None,
        store(),
        Arc::new(AllowAllPolicy),
    );

    assert_eq!(poller.command_prefix().expect("commands").as_str(), "/");
}

#[tokio::test]
async fn no_commands_passes_through() {
    let inner = Arc::new(CountingInbox::default());
    let poller = poller(inner.clone() as Arc<dyn ChannelInbox>);

    poller
        .dispatch_inbox()
        .receive(event("/stub hi"))
        .await
        .expect("receive");

    assert_eq!(inner.received_count(), 1);
}

#[tokio::test]
async fn prefixed_event_dispatches_via_command_handler() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();
    let poller = poller(inner.clone() as Arc<dyn ChannelInbox>).with_commands(
        registry(Arc::clone(&command)),
        agent_name(),
        None,
        store(),
        Arc::new(AllowAllPolicy),
    );

    poller
        .dispatch_inbox()
        .receive(event("/stub hi"))
        .await
        .expect("receive");

    assert_eq!(command.calls(), 1);
    assert_eq!(inner.received_count(), 0);
}

#[test]
fn commands_reject_pairing_inbox_outer_bypass() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();
    let result = poller(pairing_inbox(inner as Arc<dyn ChannelInbox>)).try_with_commands(
        registry(command),
        agent_name(),
        None,
        store(),
        Arc::new(AllowAllPolicy),
    );

    assert!(matches!(result, Err(CommandError::InvalidComposition)));
}

#[test]
#[should_panic(
    expected = "invalid command dispatch configuration: command dispatch must be composed inside mandatory channel gates"
)]
fn with_commands_panics_with_composition_context() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();

    let _poller = poller(pairing_inbox(inner as Arc<dyn ChannelInbox>)).with_commands(
        registry(command),
        agent_name(),
        None,
        store(),
        Arc::new(AllowAllPolicy),
    );
}

#[test]
fn commands_reject_startup_cutoff_outer_bypass() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();
    let result = poller(startup_cutoff_inbox(inner as Arc<dyn ChannelInbox>)).try_with_commands(
        registry(command),
        agent_name(),
        None,
        store(),
        Arc::new(AllowAllPolicy),
    );

    assert!(matches!(result, Err(CommandError::InvalidComposition)));
}
