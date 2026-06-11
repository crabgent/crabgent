//! Shared fixtures for the `CommandDispatchInbox` test groups.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::{
    ChannelError, ChannelInbox, ChannelKind, ChannelSink, InboundEvent, MessageRef,
    OutboundMessage, Participant, ParticipantRole,
};
use crabgent_core::MemoryScope;
use crabgent_core::{Action, Owner, PolicyDecision, PolicyHook, Subject};
use crabgent_store::memory::MemorySessionStore;

use crate::agent_name::CommandAgentName;
use crate::command::{Command, CommandCtx, CommandOutput};
use crate::error::CommandError;
use crate::handles::CommandHandles;
use crate::inbox::CommandDispatchInbox;
use crate::name::CommandName;
use crate::prefix::CommandPrefix;
use crate::registry::CommandRegistry;

#[derive(Default)]
pub(super) struct RecordingInbox {
    pub(super) calls: Mutex<usize>,
}

#[async_trait]
impl ChannelInbox for RecordingInbox {
    async fn receive(&self, _event: InboundEvent) -> Result<(), ChannelError> {
        *self.calls.lock().expect("test mutex must not be poisoned") += 1;
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct RecordingSink {
    pub(super) replies: Mutex<Vec<String>>,
    pub(super) thread_parents: Mutex<Vec<Option<String>>>,
}

#[async_trait]
impl ChannelSink for RecordingSink {
    async fn send(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        self.replies
            .lock()
            .expect("test mutex must not be poisoned")
            .push(msg.body.clone());
        self.thread_parents
            .lock()
            .expect("test mutex must not be poisoned")
            .push(msg.thread_parent.as_ref().map(|m| m.id.clone()));
        Ok(MessageRef::top_level("test", conv.clone(), "reply"))
    }

    async fn react(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        _parent: &MessageRef,
        _emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        Ok(MessageRef::top_level("test", conv.clone(), "react"))
    }
}

pub(super) struct OrderedPolicy {
    pub(super) order: Arc<Mutex<Vec<String>>>,
    pub(super) deny_outer: bool,
}

#[async_trait]
impl PolicyHook for OrderedPolicy {
    async fn allow(&self, _subject: &Subject, action: &Action) -> PolicyDecision {
        self.order
            .lock()
            .expect("test mutex must not be poisoned")
            .push(action.name().to_owned());
        if self.deny_outer && matches!(action, Action::ToolCall(_)) {
            return PolicyDecision::Deny("blocked by test policy".to_owned());
        }
        PolicyDecision::Allow
    }
}

pub(super) struct StubCommand {
    pub(super) name: CommandName,
    pub(super) order: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Command for StubCommand {
    fn name(&self) -> &CommandName {
        &self.name
    }

    fn description(&self) -> &'static str {
        "stub"
    }

    async fn policy_action(&self, _input: &str, _ctx: &CommandCtx) -> Result<Action, CommandError> {
        self.order
            .lock()
            .expect("test mutex must not be poisoned")
            .push("policy_action".to_owned());
        Ok(Action::custom("stub.inner"))
    }

    async fn execute(&self, input: &str, ctx: &CommandCtx) -> Result<CommandOutput, CommandError> {
        self.order
            .lock()
            .expect("test mutex must not be poisoned")
            .push(format!("execute:{input}"));
        ctx.send_reply("stub ok")
            .await
            .map_err(|err| CommandError::Execution(err.to_string()))?;
        Ok(CommandOutput::new("stub ok"))
    }
}

pub(super) struct SubjectCapturePolicy {
    pub(super) subjects: Arc<Mutex<Vec<(String, Subject)>>>,
}

#[async_trait]
impl PolicyHook for SubjectCapturePolicy {
    async fn allow(&self, subject: &Subject, action: &Action) -> PolicyDecision {
        self.subjects
            .lock()
            .expect("test mutex must not be poisoned")
            .push((action.name().to_owned(), subject.clone()));
        PolicyDecision::Allow
    }
}

pub(super) fn event(body: &str) -> InboundEvent {
    let conv = Owner::new("test:conv");
    InboundEvent {
        channel: "test".to_owned(),
        conv: conv.clone(),
        kind: Some(ChannelKind::Group),
        from: Participant::new("u1", ParticipantRole::Human),
        message: MessageRef::top_level("test", conv, "in1"),
        body: body.to_owned(),
        attachments: Vec::new(),
        timestamp: Utc::now(),
    }
}

/// Build the same scope `CommandDispatchInbox::dispatch_command` derives
/// from `subject_from_event` so reload-after-dispatch tests hit the same
/// `(owner, thread, scope)` row.
pub(super) fn dispatch_scope() -> MemoryScope {
    MemoryScope {
        owner: Some(Owner::new("test:conv")),
        channel: Some("test".to_owned()),
        conv: Some("test:conv".to_owned()),
        agent: Some("worker".to_owned()),
        kind: Some("group".to_owned()),
    }
}

pub(super) fn agent_name() -> CommandAgentName {
    CommandAgentName::parse("worker").expect("valid test agent name")
}

pub(super) fn command(order: Arc<Mutex<Vec<String>>>) -> Arc<dyn Command> {
    Arc::new(StubCommand {
        name: CommandName::parse("stub").expect("valid test command name"),
        order,
    })
}

pub(super) fn inbox(
    policy: Arc<dyn PolicyHook>,
    order: Arc<Mutex<Vec<String>>>,
) -> (
    CommandDispatchInbox,
    Arc<RecordingInbox>,
    Arc<RecordingSink>,
    Arc<MemorySessionStore>,
) {
    let mut registry = CommandRegistry::new();
    registry.register(command(order)).expect("register command");
    let store = Arc::new(MemorySessionStore::default());
    let handles =
        CommandHandles::new(registry, store.clone(), policy, agent_name()).expect("valid handles");
    let inner = Arc::new(RecordingInbox::default());
    let sink = Arc::new(RecordingSink::default());
    (
        CommandDispatchInbox::new(
            handles,
            CommandPrefix::default(),
            inner.clone(),
            sink.clone(),
        ),
        inner,
        sink,
        store,
    )
}
