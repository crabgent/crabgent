//! Shared command-dispatch wiring for channel adapters.

use std::sync::Arc;

use crabgent_channel::{Channel, ChannelInbox, ChannelRouter, ChannelSink};
use crabgent_core::PolicyHook;
use crabgent_core::Subject;
use crabgent_store::SessionStore;

use crate::{
    CommandAgentName, CommandDispatchInbox, CommandError, CommandHandles, CommandPrefix,
    CommandRegistry,
};

type SubjectResolver = Arc<dyn Fn(&crabgent_channel::InboundEvent) -> Subject + Send + Sync>;

/// Adapter-side command-dispatch construction.
#[derive(Clone)]
pub struct CommandWiring {
    handles: CommandHandles,
    prefix: CommandPrefix,
    subject_resolver: Option<SubjectResolver>,
}

impl CommandWiring {
    /// Build reusable command-dispatch wiring for an adapter inbox.
    pub fn try_new(
        inner: &dyn ChannelInbox,
        registry: CommandRegistry,
        agent_name: CommandAgentName,
        prefix: CommandPrefix,
        store: Arc<dyn SessionStore>,
        policy: Arc<dyn PolicyHook>,
    ) -> Result<Self, CommandError> {
        CommandDispatchInbox::ensure_wrap_allowed(inner)?;
        Ok(Self {
            handles: CommandHandles::new(registry, store, policy, agent_name)?,
            prefix,
            subject_resolver: None,
        })
    }

    /// Borrow the configured adapter command prefix.
    #[must_use]
    pub const fn prefix(&self) -> &CommandPrefix {
        &self.prefix
    }

    /// Install a custom subject resolver for command policy context.
    #[must_use]
    pub fn with_subject_resolver<F>(mut self, f: F) -> Self
    where
        F: Fn(&crabgent_channel::InboundEvent) -> Subject + Send + Sync + 'static,
    {
        self.subject_resolver = Some(Arc::new(f));
        self
    }

    /// Wrap `inner` in command dispatch using `channel` as the reply path.
    #[must_use]
    pub fn wrap_inbox(
        &self,
        inner: Arc<dyn ChannelInbox>,
        channel: Arc<dyn Channel>,
    ) -> Arc<dyn ChannelInbox> {
        let sink: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new().with_channel(channel));
        let inbox =
            CommandDispatchInbox::new(self.handles.clone(), self.prefix.clone(), inner, sink);
        let inbox = if let Some(resolve) = &self.subject_resolver {
            let resolve = Arc::clone(resolve);
            inbox.with_subject_resolver(move |event| resolve(event))
        } else {
            inbox
        };
        Arc::new(inbox)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use crabgent_channel::{
        ChannelError, ChannelInbox, ChannelKind, InboundEvent, MessageRef, OutboundMessage,
        Participant, ParticipantRole,
    };
    use crabgent_core::{Action, AllowAllPolicy, Owner, Subject};
    use crabgent_store::memory::MemorySessionStore;

    use super::*;
    use crate::{Command, CommandAgentName, CommandCtx, CommandName, CommandOutput};

    struct BlockingInbox;

    #[async_trait]
    impl ChannelInbox for BlockingInbox {
        async fn receive(&self, _event: InboundEvent) -> Result<(), ChannelError> {
            Ok(())
        }

        fn blocks_outer_command_dispatch(&self) -> bool {
            true
        }
    }

    #[derive(Default)]
    struct RecordingInbox {
        events: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ChannelInbox for RecordingInbox {
        async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
            self.events
                .lock()
                .expect("test mutex must not be poisoned")
                .push(event.body);
            Ok(())
        }
    }

    struct StubCommand {
        name: CommandName,
    }

    #[async_trait]
    impl Command for StubCommand {
        fn name(&self) -> &CommandName {
            &self.name
        }

        fn description(&self) -> &'static str {
            "stub command"
        }

        async fn policy_action(
            &self,
            _input: &str,
            _ctx: &CommandCtx,
        ) -> Result<Action, CommandError> {
            Ok(Action::custom("command.stub"))
        }

        async fn execute(
            &self,
            input: &str,
            ctx: &CommandCtx,
        ) -> Result<CommandOutput, CommandError> {
            let reply = format!("stub: {input}");
            ctx.send_reply(reply.clone())
                .await
                .map_err(|err| CommandError::Execution(err.to_string()))?;
            Ok(CommandOutput::new(reply))
        }
    }

    #[derive(Default)]
    struct RecordingChannel {
        sent: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Channel for RecordingChannel {
        fn name(&self) -> &'static str {
            "test"
        }

        async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, ChannelError> {
            Ok(ChannelKind::Group)
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
            msg: &OutboundMessage,
        ) -> Result<MessageRef, ChannelError> {
            self.sent
                .lock()
                .expect("test mutex must not be poisoned")
                .push(msg.body.clone());
            Ok(MessageRef::top_level("test", conv.clone(), "reply"))
        }
    }

    fn registry() -> CommandRegistry {
        CommandRegistry::new()
            .with_command(Arc::new(StubCommand {
                name: CommandName::parse("stub").expect("valid test command name"),
            }))
            .expect("registry accepts stub command")
    }

    fn agent_name() -> CommandAgentName {
        CommandAgentName::parse("worker").expect("valid test agent name")
    }

    fn wiring(inner: &dyn ChannelInbox, prefix: CommandPrefix) -> CommandWiring {
        CommandWiring::try_new(
            inner,
            registry(),
            agent_name(),
            prefix,
            Arc::new(MemorySessionStore::default()),
            Arc::new(AllowAllPolicy),
        )
        .expect("valid command wiring")
    }

    fn event(body: &str) -> InboundEvent {
        let conv = Owner::new("test:conv");
        InboundEvent {
            channel: "test".to_owned(),
            conv: conv.clone(),
            kind: Some(ChannelKind::Group),
            from: Participant::new("u1", ParticipantRole::Human),
            message: MessageRef::top_level("test", conv, "in1"),
            body: body.to_owned(),
            attachments: Vec::new(),
            timestamp: crabgent_store::Utc::now(),
        }
    }

    #[test]
    fn try_new_rejects_invalid_outer_composition() {
        let result = CommandWiring::try_new(
            &BlockingInbox,
            registry(),
            agent_name(),
            CommandPrefix::default(),
            Arc::new(MemorySessionStore::default()),
            Arc::new(AllowAllPolicy),
        );

        assert!(matches!(result, Err(CommandError::InvalidComposition)));
    }

    #[test]
    fn prefix_returns_configured_prefix() {
        let inner = RecordingInbox::default();
        let wiring = wiring(
            &inner,
            CommandPrefix::parse("!").expect("valid test command prefix"),
        );

        assert_eq!(wiring.prefix().as_str(), "!");
    }

    #[tokio::test]
    async fn wrap_inbox_dispatches_command_through_channel_router() {
        let inner = Arc::new(RecordingInbox::default());
        let channel = Arc::new(RecordingChannel::default());
        let wiring = wiring(inner.as_ref(), CommandPrefix::default());
        let inbox = wiring.wrap_inbox(
            inner.clone() as Arc<dyn ChannelInbox>,
            channel.clone() as Arc<dyn Channel>,
        );

        inbox
            .receive(event("/stub value"))
            .await
            .expect("receive ok");

        assert!(
            inner
                .events
                .lock()
                .expect("test mutex must not be poisoned")
                .is_empty()
        );
        assert_eq!(
            channel
                .sent
                .lock()
                .expect("test mutex must not be poisoned")
                .as_slice(),
            &["stub: value".to_owned()]
        );
    }
}
