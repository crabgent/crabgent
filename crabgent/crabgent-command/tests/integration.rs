use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::{
    ChannelError, ChannelInbox, ChannelKind, InboundEvent, MessageRef, Participant, ParticipantRole,
};
use crabgent_command::{
    Command, CommandAgentName, CommandCtx, CommandDispatchInbox, CommandError, CommandHandles,
    CommandName, CommandOutput, CommandPrefix, CommandRegistry,
};
use crabgent_core::MemoryScope;
use crabgent_core::{Action, AllowAllPolicy, ContentBlock, DenyAllPolicy, Message, Owner};
use crabgent_store::SessionStore;
use crabgent_store::memory::MemorySessionStore;
use crabgent_test_support::RecordingSink;
use std::sync::atomic::{AtomicUsize, Ordering};

struct EchoCommand {
    name: CommandName,
}

#[async_trait]
impl Command for EchoCommand {
    fn name(&self) -> &CommandName {
        &self.name
    }

    fn description(&self) -> &'static str {
        "echoes command input"
    }

    async fn policy_action(&self, _input: &str, _ctx: &CommandCtx) -> Result<Action, CommandError> {
        Ok(Action::custom("echo.inner"))
    }

    async fn execute(&self, input: &str, ctx: &CommandCtx) -> Result<CommandOutput, CommandError> {
        let reply = format!("echo: {input}");
        ctx.send_reply(reply.clone())
            .await
            .map_err(|err| CommandError::Execution(err.to_string()))?;
        Ok(CommandOutput::new(reply))
    }
}

#[derive(Default)]
struct StubInbox {
    calls: Mutex<usize>,
    shutdowns: Mutex<usize>,
}

#[async_trait]
impl ChannelInbox for StubInbox {
    async fn receive(&self, _event: InboundEvent) -> Result<(), ChannelError> {
        *self.calls.lock().expect("test mutex must not be poisoned") += 1;
        Ok(())
    }

    async fn shutdown(&self, _grace: std::time::Duration) {
        *self
            .shutdowns
            .lock()
            .expect("test mutex must not be poisoned") += 1;
    }
}

fn event(body: &str) -> InboundEvent {
    let conv = Owner::new("stub:conv");
    InboundEvent {
        channel: "stub".to_owned(),
        conv: conv.clone(),
        kind: Some(ChannelKind::Group),
        from: Participant::new("u1", ParticipantRole::Human),
        message: MessageRef::top_level("stub", conv, "in-1"),
        body: body.to_owned(),
        attachments: Vec::new(),
        timestamp: Utc::now(),
    }
}

/// Mirror the scope `CommandDispatchInbox::dispatch_command` derives from
/// the inbound event so reload-after-dispatch lookups hit the same row.
fn stub_dispatch_scope() -> MemoryScope {
    MemoryScope {
        owner: Some(Owner::new("stub:conv")),
        channel: Some("stub".to_owned()),
        conv: Some("stub:conv".to_owned()),
        agent: Some("worker".to_owned()),
        kind: Some("group".to_owned()),
    }
}

fn agent_name() -> CommandAgentName {
    CommandAgentName::parse("worker").expect("valid test agent name")
}

#[tokio::test]
async fn end_to_end_dispatch_with_stub_channel_and_store() {
    let mut registry = CommandRegistry::new();
    registry
        .register(Arc::new(EchoCommand {
            name: CommandName::parse("echo").expect("valid test command name"),
        }))
        .expect("register command");
    let store = Arc::new(MemorySessionStore::default());
    let handles = CommandHandles::new(
        registry,
        store.clone(),
        Arc::new(AllowAllPolicy),
        agent_name(),
    )
    .expect("valid handles");
    let inner = Arc::new(StubInbox::default());
    let sink = Arc::new(RecordingSink::new());
    let inbox = CommandDispatchInbox::new(
        handles,
        CommandPrefix::default(),
        inner.clone(),
        sink.clone(),
    );

    inbox
        .receive(event("/echo hello"))
        .await
        .expect("dispatch succeeds");

    assert_eq!(
        *inner.calls.lock().expect("test mutex must not be poisoned"),
        0
    );
    assert_eq!(sink.sent().as_slice(), &["echo: hello".to_owned()]);

    let session = store
        .find_or_create(&Owner::new("stub:conv"), None, &stub_dispatch_scope())
        .await
        .expect("session persisted");
    assert_eq!(session.messages.len(), 2);
    assert!(
        matches!(&session.messages[0], Message::User { content, ..} if matches!(&content[0], ContentBlock::Text { text } if text == "/echo hello"))
    );
    assert!(
        matches!(&session.messages[1], Message::Assistant { text, .. } if text == "echo: hello")
    );
}

struct CountingCommand {
    name: CommandName,
    execute_count: Arc<AtomicUsize>,
}

#[async_trait]
impl Command for CountingCommand {
    fn name(&self) -> &CommandName {
        &self.name
    }

    fn description(&self) -> &'static str {
        "counts execute invocations"
    }

    async fn policy_action(&self, _input: &str, _ctx: &CommandCtx) -> Result<Action, CommandError> {
        Ok(Action::custom("counting.inner"))
    }

    async fn execute(&self, input: &str, _ctx: &CommandCtx) -> Result<CommandOutput, CommandError> {
        self.execute_count.fetch_add(1, Ordering::SeqCst);
        Ok(CommandOutput::new(format!("echo: {input}")))
    }
}

#[tokio::test]
async fn deny_policy_short_circuits_before_execute_and_sends_safe_reply() {
    let execute_count = Arc::new(AtomicUsize::new(0));
    let mut registry = CommandRegistry::new();
    registry
        .register(Arc::new(CountingCommand {
            name: CommandName::parse("count").expect("valid command name"),
            execute_count: execute_count.clone(),
        }))
        .expect("register counting command");
    let store = Arc::new(MemorySessionStore::default());
    let handles = CommandHandles::new(
        registry,
        store.clone(),
        Arc::new(DenyAllPolicy),
        agent_name(),
    )
    .expect("valid handles");
    let inner = Arc::new(StubInbox::default());
    let sink = Arc::new(RecordingSink::new());
    let inbox = CommandDispatchInbox::new(
        handles,
        CommandPrefix::default(),
        inner.clone(),
        sink.clone(),
    );

    inbox
        .receive(event("/count payload"))
        .await
        .expect("dispatch returns Ok even on policy deny");

    // Deny short-circuit: execute() never runs.
    assert_eq!(execute_count.load(Ordering::SeqCst), 0);
    // The decorator does not delegate to the inner inbox after handling
    // the prefixed command.
    assert_eq!(
        *inner.calls.lock().expect("test mutex must not be poisoned"),
        0
    );
    // Single safe-reply body lands on the sink with the policy reason.
    let bodies = sink.sent();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0], "denied by DenyAllPolicy");

    // No session bookkeeping for an action that was rejected before
    // execute.
    let session = store
        .find_or_create(&Owner::new("stub:conv"), None, &stub_dispatch_scope())
        .await
        .expect("session lookup succeeds");
    assert!(session.messages.is_empty());
}

#[tokio::test]
async fn command_dispatch_inbox_forwards_shutdown_to_inner() {
    let mut registry = CommandRegistry::new();
    registry
        .register(Arc::new(EchoCommand {
            name: CommandName::parse("echo").expect("valid test command name"),
        }))
        .expect("register command");
    let store = Arc::new(MemorySessionStore::default());
    let handles = CommandHandles::new(registry, store, Arc::new(AllowAllPolicy), agent_name())
        .expect("valid handles");
    let inner = Arc::new(StubInbox::default());
    let sink = Arc::new(RecordingSink::new());
    let inbox = CommandDispatchInbox::new(handles, CommandPrefix::default(), inner.clone(), sink);

    inbox.shutdown(std::time::Duration::ZERO).await;

    assert_eq!(
        *inner
            .shutdowns
            .lock()
            .expect("test mutex must not be poisoned"),
        1,
        "decorator must forward shutdown to inner"
    );
}
