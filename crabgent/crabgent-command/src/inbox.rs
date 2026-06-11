//! `ChannelInbox` decorator that dispatches prefixed commands.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::{
    ChannelError, ChannelInbox, ChannelSink, InboundEvent, OutboundMessage, attr_keys,
    subject_from_inbound_event,
};
use crabgent_core::MemoryScope;
use crabgent_core::{Action, ContentBlock, Message, PolicyDecision, Subject};

use crate::command::{CommandCtx, CommandOutput};
use crate::error::CommandError;
use crate::handles::CommandHandles;
use crate::name::CommandName;
use crate::prefix::CommandPrefix;

const AGENT_ATTR: &str = "agent";

type SubjectResolver = Arc<dyn Fn(&InboundEvent) -> Subject + Send + Sync>;

/// Inbox decorator that dispatches prefixed commands before the kernel inbox.
pub struct CommandDispatchInbox {
    handles: CommandHandles,
    prefix: CommandPrefix,
    inner: Arc<dyn ChannelInbox>,
    sink: Arc<dyn ChannelSink>,
    subject_resolver: Option<SubjectResolver>,
}

impl CommandDispatchInbox {
    /// Validate that adapter-side wrapping cannot bypass mandatory
    /// channel gates already present in the supplied inbox stack.
    pub fn ensure_wrap_allowed(inner: &dyn ChannelInbox) -> Result<(), CommandError> {
        if inner.blocks_outer_command_dispatch() {
            return Err(CommandError::InvalidComposition);
        }
        Ok(())
    }

    /// Build a command-dispatch decorator.
    #[must_use]
    pub fn new(
        handles: CommandHandles,
        prefix: CommandPrefix,
        inner: Arc<dyn ChannelInbox>,
        sink: Arc<dyn ChannelSink>,
    ) -> Self {
        Self {
            handles,
            prefix,
            inner,
            sink,
            subject_resolver: None,
        }
    }

    /// Install a custom subject resolver for command policy context.
    #[must_use]
    pub fn with_subject_resolver<F>(mut self, f: F) -> Self
    where
        F: Fn(&InboundEvent) -> Subject + Send + Sync + 'static,
    {
        self.subject_resolver = Some(Arc::new(f));
        self
    }

    async fn dispatch_command(
        &self,
        event: InboundEvent,
        parsed: ParsedCommand,
    ) -> Result<(), ChannelError> {
        let Some(command) = self.handles.registry().get(&parsed.name) else {
            return self.inner.receive(event).await;
        };
        let subject = self.subject_from_event(&event)?;

        let outer = Action::tool(parsed.name.to_string());
        if let Err(err) = self.allow(&subject, &outer).await {
            return self.reply_error(&subject, &event, &err).await;
        }

        let mut scope = MemoryScope::from_subject(&subject);
        scope.owner = Some(event.conv.clone());
        scope.agent = Some(self.handles.agent_name().as_str().to_owned());
        let session = self
            .handles
            .store()
            .find_or_create(&event.conv, None, &scope)
            .await
            .map_err(ChannelError::adapter)?;
        let ctx = CommandCtx::new(
            subject.clone(),
            session.id.clone(),
            event.clone(),
            Arc::clone(&self.sink),
        );
        let inner = match command.policy_action(&parsed.input, &ctx).await {
            Ok(action) => action,
            Err(err) => return self.reply_error(&subject, &event, &err).await,
        };
        if let Err(err) = self.allow(&subject, &inner).await {
            return self.reply_error(&subject, &event, &err).await;
        }

        match command.execute(&parsed.input, &ctx).await {
            Ok(output) => self.record_success(&event, &session.id, output).await,
            Err(err) => self.reply_error(&subject, &event, &err).await,
        }
    }

    async fn allow(&self, subject: &Subject, action: &Action) -> Result<(), CommandError> {
        match self.handles.policy().allow(subject, action).await {
            PolicyDecision::Allow => Ok(()),
            PolicyDecision::Deny(reason) => Err(CommandError::PermissionDenied(reason)),
        }
    }

    async fn reply_error(
        &self,
        subject: &Subject,
        event: &InboundEvent,
        err: &CommandError,
    ) -> Result<(), ChannelError> {
        let msg =
            OutboundMessage::new(err.safe_reply()).with_metadata("channel", event.channel.as_str());
        self.sink.send(subject, &event.conv, &msg).await?;
        Ok(())
    }

    /// Persist the command exchange via load-append-`save_messages`.
    ///
    /// Known limitation: there is no lock held across load..save. If a
    /// kernel turn on the same conv appends to this session row between the
    /// `load` and `save_messages` here, that concurrent append is lost
    /// (last-writer-wins on the whole `messages` vector, the same tradeoff
    /// `SessionStore::save_messages` documents for `SessionPersistHook`).
    ///
    /// This is bounded, not eliminated, by the dispatch model. Telegram
    /// (`poller::tick_once`) and Matrix (`sync::receive_message`) drive
    /// `ChannelInbox::receive` serially per poll/sync round, so a command
    /// and a normal inbound on the same conv from the same adapter do not
    /// interleave there. Slack fans events out concurrently
    /// (`ListenerRegistry::dispatch` spawns a task per event), so on Slack a
    /// command and an overlapping kernel run on the same conv can race; the
    /// window is small (commands are infrequent, human-typed) and the impact
    /// is a lost message append, not corruption. Per-conv serialization or a
    /// per-row lock would close it but is out of scope here: it would
    /// regress the dispatch hot path for an edge that the deployments in use
    /// (single-node in-memory/SQLite) have not hit.
    async fn record_success(
        &self,
        event: &InboundEvent,
        session_id: &crabgent_store::SessionId,
        output: CommandOutput,
    ) -> Result<(), ChannelError> {
        let store = self.handles.store();
        let Some(mut session) = store
            .load(session_id)
            .await
            .map_err(ChannelError::adapter)?
        else {
            return Err(ChannelError::adapter("command session disappeared"));
        };
        session.messages.push(user_message(event));
        session.messages.push(Message::Assistant {
            text: output.reply,
            tool_calls: Vec::new(),
        });
        store
            .save_messages(session_id, &session.messages, Utc::now())
            .await
            .map_err(ChannelError::adapter)?;
        Ok(())
    }

    fn subject_from_event(&self, event: &InboundEvent) -> Result<Subject, ChannelError> {
        let subject = if let Some(resolve) = &self.subject_resolver {
            resolve(event)
        } else {
            subject_from_event(event)?
        };
        Ok(subject.with_attr(AGENT_ATTR, self.handles.agent_name().as_str()))
    }
}

#[async_trait]
impl ChannelInbox for CommandDispatchInbox {
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        let Some(parsed) = parse_command(event.body.as_str(), &self.prefix) else {
            return self.inner.receive(event).await;
        };
        self.dispatch_command(event, parsed).await
    }

    crabgent_channel::forward_channel_inbox_methods!(inner);
}

struct ParsedCommand {
    name: CommandName,
    input: String,
}

fn parse_command(body: &str, prefix: &CommandPrefix) -> Option<ParsedCommand> {
    let rest = body.strip_prefix(prefix.as_str())?.trim_start();
    if rest.is_empty() {
        return None;
    }
    let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (name, input) = rest.split_at(name_end);
    let input = input.trim_start().to_owned();
    let name = CommandName::parse(name).ok()?;
    Some(ParsedCommand { name, input })
}

fn subject_from_event(event: &InboundEvent) -> Result<Subject, ChannelError> {
    Ok(subject_from_inbound_event(event, None)?
        .with_attr(attr_keys::PARTICIPANT_ID, event.from.id.as_str()))
}

fn user_message(event: &InboundEvent) -> Message {
    let mut content = vec![ContentBlock::Text {
        text: event.body.clone(),
    }];
    content.extend(event.attachments.clone());
    Message::user_at(content, event.timestamp)
}

#[cfg(test)]
mod tests;
