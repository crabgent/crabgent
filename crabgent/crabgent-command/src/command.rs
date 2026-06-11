//! Command trait and execution context.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_channel::{ChannelError, ChannelSink, InboundEvent, MessageRef, OutboundMessage};
use crabgent_core::{Action, Subject};
use crabgent_store::SessionId;
use tokio_util::sync::CancellationToken;

use crate::error::CommandError;
use crate::name::CommandName;

/// Context passed to a command execution.
#[derive(Clone)]
pub struct CommandCtx {
    subject: Subject,
    session_id: SessionId,
    event: InboundEvent,
    sink: Arc<dyn ChannelSink>,
    cancel: Option<CancellationToken>,
}

impl CommandCtx {
    /// Build a command context.
    pub fn new(
        subject: Subject,
        session_id: SessionId,
        event: InboundEvent,
        sink: Arc<dyn ChannelSink>,
    ) -> Self {
        Self {
            subject,
            session_id,
            event,
            sink,
            cancel: None,
        }
    }

    /// Attach a cancellation token.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Borrow the policy subject.
    #[must_use]
    pub const fn subject(&self) -> &Subject {
        &self.subject
    }

    /// Borrow the current session id.
    #[must_use]
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Borrow the inbound event.
    #[must_use]
    pub const fn event(&self) -> &InboundEvent {
        &self.event
    }

    /// Borrow the cancellation token, if set.
    #[must_use]
    pub const fn cancel(&self) -> Option<&CancellationToken> {
        self.cancel.as_ref()
    }

    /// Send a plain-text reply to the event conversation.
    ///
    /// Replies are posted as channel-level messages, not as thread
    /// replies. Command output is gateway-side state that addresses the
    /// whole conversation (the model registry, the compacted session,
    /// ...). Forcing a thread fragments DM chats where users never see
    /// the threaded reply, so the reply travels in the same surface as
    /// the prompt that invoked it.
    pub async fn send_reply(&self, body: impl Into<String>) -> Result<MessageRef, ChannelError> {
        let msg = OutboundMessage::new(body).with_metadata("channel", self.event.channel.as_str());
        self.sink.send(&self.subject, &self.event.conv, &msg).await
    }
}

/// Command execution result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Text reply that was delivered to the channel and should be persisted.
    pub reply: String,
}

impl CommandOutput {
    /// Build a command output from reply text.
    pub fn new(reply: impl Into<String>) -> Self {
        Self {
            reply: reply.into(),
        }
    }
}

/// A channel command.
///
/// Commands are responsible for sending their own success reply via
/// [`CommandCtx::send_reply`]. The dispatch inbox uses the returned
/// [`CommandOutput::reply`] only for `SessionStore::save_messages`.
#[async_trait]
pub trait Command: Send + Sync {
    /// Registry name.
    fn name(&self) -> &CommandName;

    /// Human-readable description.
    fn description(&self) -> &'static str;

    /// Typed action for the command operation being requested.
    async fn policy_action(&self, input: &str, ctx: &CommandCtx) -> Result<Action, CommandError>;

    /// Execute the command.
    async fn execute(&self, input: &str, ctx: &CommandCtx) -> Result<CommandOutput, CommandError>;
}
