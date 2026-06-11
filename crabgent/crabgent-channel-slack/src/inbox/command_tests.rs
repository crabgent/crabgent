use std::sync::Arc;

use async_trait::async_trait;
use crabgent_channel::{
    ChannelInbox, ChannelKind, ChannelRouter, InboundEvent, MemoryPairingStore, MessageRef,
    PairingInbox, Participant, ParticipantRole, StartupCutoffInbox,
};
use crabgent_command::{CommandAgentName, CommandError, CommandRegistry, SessionStore};
use crabgent_core::{AllowAllPolicy, Owner};
use crabgent_store::memory::MemorySessionStore;
use crabgent_test_support::{CountingInbox, StubCommand};
use secrecy::SecretString;

use crate::SlackHttpClient;
use crate::config::SlackConfig;
use crate::connection::{SocketFactory, SocketModePool};
use crate::dispatch::ListenerRegistry;
use crate::error::SlackError;
use crate::events::SocketModeEnvelope;
use crate::inbox::{DEFAULT_COMMAND_PREFIX, SlackInbox};
use crate::socket_mode::SocketModeClient;

fn stub_command() -> Arc<StubCommand> {
    Arc::new(StubCommand::new("stub").without_sink_reply())
}

struct NoopSocketModeClient;

#[async_trait]
impl SocketModeClient for NoopSocketModeClient {
    async fn connect(&self, _url: &str) -> Result<(), SlackError> {
        Ok(())
    }

    async fn next_envelope(&self) -> Result<SocketModeEnvelope, SlackError> {
        Err(SlackError::Internal("unused test socket".to_owned()))
    }

    async fn ack(&self, _envelope_id: &str) -> Result<(), SlackError> {
        Ok(())
    }
}

fn slack_inbox(inner: Arc<dyn ChannelInbox>) -> SlackInbox {
    let config = SlackConfig::new(
        SecretString::from("xapp-test".to_owned()),
        SecretString::from("xoxb-test".to_owned()),
    )
    .expect("slack config");
    let http = Arc::new(SlackHttpClient::new(config).expect("slack http client"));
    let registry = Arc::new(ListenerRegistry::new());
    let factory: SocketFactory = Arc::new(|| {
        let socket: Arc<dyn SocketModeClient> = Arc::new(NoopSocketModeClient);
        socket
    });
    let pool =
        Arc::new(SocketModePool::new(http, factory, Arc::clone(&registry)).with_connections(1));
    SlackInbox::new(pool, registry, inner)
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
    let conv = Owner::new("slack:T123/C123");
    InboundEvent {
        channel: "slack".to_owned(),
        conv: conv.clone(),
        kind: Some(ChannelKind::Group),
        from: Participant::new("U123", ParticipantRole::Human),
        message: MessageRef::top_level("slack", conv, "1.1"),
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

#[tokio::test]
async fn builder_with_commands_wraps_inbox() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();
    let inbox = slack_inbox(inner as Arc<dyn ChannelInbox>).with_commands(
        registry(Arc::clone(&command)),
        agent_name(),
        None,
        store(),
        Arc::new(AllowAllPolicy),
    );

    assert_eq!(inbox.command_prefix().expect("commands").as_str(), "/");
}

#[tokio::test]
async fn no_commands_passes_through() {
    let inner = Arc::new(CountingInbox::default());
    let inbox = slack_inbox(inner.clone() as Arc<dyn ChannelInbox>);

    inbox
        .inbox()
        .receive(event("/stub hi"))
        .await
        .expect("receive");

    assert_eq!(inner.received_count(), 1);
}

#[tokio::test]
async fn prefixed_event_dispatches_via_command_handler() {
    let inner = Arc::new(CountingInbox::default());
    let command = stub_command();
    let inbox = slack_inbox(inner.clone() as Arc<dyn ChannelInbox>).with_commands(
        registry(Arc::clone(&command)),
        agent_name(),
        None,
        store(),
        Arc::new(AllowAllPolicy),
    );

    inbox
        .inbox()
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
    let result = slack_inbox(pairing_inbox(inner as Arc<dyn ChannelInbox>)).try_with_commands(
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

    let _inbox = slack_inbox(pairing_inbox(inner as Arc<dyn ChannelInbox>)).with_commands(
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
    let result = slack_inbox(startup_cutoff_inbox(inner as Arc<dyn ChannelInbox>))
        .try_with_commands(
            registry(command),
            agent_name(),
            None,
            store(),
            Arc::new(AllowAllPolicy),
        );

    assert!(matches!(result, Err(CommandError::InvalidComposition)));
}
