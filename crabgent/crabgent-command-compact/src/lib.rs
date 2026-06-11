//! Command adapter for compacting the current persisted session.

#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_command::{Command, CommandCtx, CommandError, CommandName, CommandOutput};
use crabgent_core::Action;
use crabgent_hook_compact::{CompactError, CompactHook};
use crabgent_store::SessionStore;

const COMMAND_NAME: &str = "compact";
const DESCRIPTION: &str = "Compact the current persisted session.";
const SUCCESS_REPLY: &str = "Session compacted.";

/// Command that runs [`CompactHook::compact_session`] for the current session.
pub struct CompactCommand {
    name: CommandName,
    hook: Arc<CompactHook>,
    store: Arc<dyn SessionStore>,
}

impl CompactCommand {
    /// Build a compact command.
    #[must_use]
    pub fn new(hook: Arc<CompactHook>, store: Arc<dyn SessionStore>) -> Self {
        Self {
            name: COMMAND_NAME
                .parse()
                .expect("static compact command name is valid"),
            hook,
            store,
        }
    }
}

#[async_trait]
impl Command for CompactCommand {
    fn name(&self) -> &CommandName {
        &self.name
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    async fn policy_action(&self, _input: &str, _ctx: &CommandCtx) -> Result<Action, CommandError> {
        Ok(Action::custom("compact.session"))
    }

    async fn execute(&self, _input: &str, ctx: &CommandCtx) -> Result<CommandOutput, CommandError> {
        self.hook
            .compact_session(
                Arc::clone(&self.store),
                ctx.session_id().clone(),
                ctx.subject().clone(),
            )
            .await
            .map_err(map_compact_error)?;
        ctx.send_reply(SUCCESS_REPLY)
            .await
            .map_err(|err| CommandError::Execution(format!("compact reply send failed: {err}")))?;
        Ok(CommandOutput::new(SUCCESS_REPLY))
    }
}

fn map_compact_error(error: CompactError) -> CommandError {
    match error {
        CompactError::Store(error) => CommandError::Store(error),
        other => CommandError::Execution(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crabgent_channel::{ChannelSink, InboundEvent, MessageRef, Participant, ParticipantRole};
    use crabgent_core::{MemoryScope, Message, Owner, Subject};
    use crabgent_store::memory::MemorySessionStore;
    use crabgent_store::{SessionId, SessionStore};
    use crabgent_test_support::{RecordingSink, StubProvider, user_msg};

    use super::*;

    fn inbound_event() -> InboundEvent {
        let conv = Owner::new("u");
        InboundEvent {
            channel: "test".into(),
            conv: conv.clone(),
            kind: None,
            from: Participant::new("alice", ParticipantRole::Human),
            message: MessageRef::top_level("test", conv, "msg-1"),
            body: "/compact".into(),
            attachments: Vec::new(),
            timestamp: crabgent_store::Utc::now(),
        }
    }

    async fn command_ctx(
        store: &Arc<MemorySessionStore>,
        messages: Vec<Message>,
        sink: Arc<dyn ChannelSink>,
    ) -> CommandCtx {
        let mut session = store
            .find_or_create(&Owner::new("u"), None, &MemoryScope::default())
            .await
            .expect("session created");
        session.messages = messages;
        store.save(&session).await.expect("session saved");
        CommandCtx::new(Subject::new("u"), session.id, inbound_event(), sink)
    }

    fn command(store: Arc<MemorySessionStore>) -> CompactCommand {
        let provider = Arc::new(StubProvider::with_text("summary"));
        let hook = Arc::new(
            CompactHook::new(provider, "summary-model")
                .with_max_messages(1)
                .with_keep_recent_messages(1),
        );
        let store_dyn: Arc<dyn SessionStore> = store;
        CompactCommand::new(hook, store_dyn)
    }

    #[tokio::test]
    async fn compact_command_invokes_hook_compact_session() {
        let store = Arc::new(MemorySessionStore::default());
        let sink = Arc::new(RecordingSink::default());
        let ctx = command_ctx(&store, vec![user_msg("old"), user_msg("latest")], sink).await;
        let session_id = ctx.session_id().clone();

        command(Arc::clone(&store))
            .execute("", &ctx)
            .await
            .expect("compact command succeeds");

        let loaded = store
            .load(&session_id)
            .await
            .expect("load succeeds")
            .expect("session exists");
        assert_eq!(loaded.messages.len(), 2);
    }

    #[tokio::test]
    async fn compact_command_returns_text_reply_on_success() {
        let store = Arc::new(MemorySessionStore::default());
        let sink = Arc::new(RecordingSink::default());
        let ctx = command_ctx(
            &store,
            vec![user_msg("old"), user_msg("latest")],
            Arc::clone(&sink) as Arc<dyn ChannelSink>,
        )
        .await;

        let output = command(store)
            .execute("", &ctx)
            .await
            .expect("compact command succeeds");

        assert_eq!(output.reply, SUCCESS_REPLY);
        assert_eq!(sink.sent().as_slice(), [SUCCESS_REPLY]);
    }

    #[tokio::test]
    async fn compact_command_returns_safe_error_reply_on_hook_error() {
        let store = Arc::new(MemorySessionStore::default());
        let sink = Arc::new(RecordingSink::default());
        let ctx = CommandCtx::new(
            Subject::new("u"),
            SessionId::new(),
            inbound_event(),
            sink as Arc<dyn ChannelSink>,
        );

        let err = command(store)
            .execute("", &ctx)
            .await
            .expect_err("missing session errors");

        assert_eq!(err.safe_reply(), "command failed");
    }
}
