//! Outbound side: `ChannelSink` trait + `ChannelRouter`.
//!
//! `ChannelSink` is the abstraction the kernel (or a tool, or a cron
//! delivery) calls to push a message into a channel. `ChannelRouter`
//! owns a `HashMap<channel_name, Arc<dyn Channel>>` and dispatches
//! by adapter name.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::owner::Owner;
use crabgent_core::subject::Subject;

use crate::channel::{Channel, ReadMessage};
use crate::envelope::{MessageRef, OutboundMessage};
use crate::error::ChannelError;
use crate::participant::ParticipantId;

/// Send-side abstraction over channels.
///
/// One implementation services many adapters: `ChannelRouter` is the
/// canonical implementation, but consumer code may inject a custom
/// sink (audit-logging wrapper, multi-cast fan-out, ...) without
/// touching the channels themselves.
#[async_trait]
pub trait ChannelSink: Send + Sync {
    /// Send `msg` to `conv` via the channel selected from the
    /// dispatcher. The implementation chooses how to map `conv` (or
    /// `msg.metadata`) to a concrete adapter.
    async fn send(
        &self,
        ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError>;

    /// Post `emoji` as a reaction to `parent` in `conv`.
    async fn react(
        &self,
        ctx: &Subject,
        conv: &Owner,
        parent: &MessageRef,
        emoji: &str,
    ) -> Result<MessageRef, ChannelError>;

    /// Edit `target` in `conv`.
    async fn edit(
        &self,
        ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
        new_text: &str,
    ) -> Result<(), ChannelError> {
        let _ = (ctx, conv, target, new_text);
        Err(ChannelError::Unsupported("edit"))
    }

    /// Delete `target` in `conv`.
    async fn delete(
        &self,
        ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
    ) -> Result<(), ChannelError> {
        let _ = (ctx, conv, target);
        Err(ChannelError::Unsupported("delete"))
    }

    /// Upload `bytes` as `filename` in `conv`.
    async fn upload(
        &self,
        ctx: &Subject,
        conv: &Owner,
        filename: &str,
        bytes: Vec<u8>,
        comment: Option<&str>,
        thread_parent: Option<&MessageRef>,
    ) -> Result<MessageRef, ChannelError> {
        let _ = (ctx, conv, filename, bytes, comment, thread_parent);
        Err(ChannelError::Unsupported("upload"))
    }

    /// Read messages in `conv`, optionally scoped to a thread.
    async fn read(
        &self,
        ctx: &Subject,
        conv: &Owner,
        thread_parent: Option<&MessageRef>,
        limit: usize,
    ) -> Result<Vec<ReadMessage>, ChannelError> {
        let _ = (ctx, conv, thread_parent, limit);
        Err(ChannelError::Unsupported("read"))
    }

    /// Notify `recipient` out-of-band by opening or reusing a direct
    /// conversation with that user.
    ///
    /// Unlike `send`, this method takes no `conv: &Owner`: the caller
    /// addresses a participant directly. The routing implementation
    /// must therefore rely on `msg.metadata["channel"]` (no
    /// conv-prefix fallback exists).
    async fn notify_user(
        &self,
        ctx: &Subject,
        recipient: &ParticipantId,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let _ = (ctx, recipient, msg);
        Err(ChannelError::Unsupported("notify_user"))
    }
}

/// Router that dispatches `ChannelSink::send` to one of many
/// `Channel` adapters based on adapter name.
///
/// The `metadata["channel"]` entry on `OutboundMessage` selects the
/// adapter. If the metadata is missing the router falls back to
/// trying to extract a `<adapter>:<rest>` prefix from `conv`.
#[derive(Default)]
pub struct ChannelRouter {
    channels: HashMap<String, Arc<dyn Channel>>,
}

impl ChannelRouter {
    /// Build an empty router.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a `Channel` implementation under its `name()`.
    ///
    /// If a channel with the same name was registered before, it is
    /// replaced.
    #[must_use]
    pub fn with_channel(mut self, channel: Arc<dyn Channel>) -> Self {
        self.channels.insert(channel.name().to_owned(), channel);
        self
    }

    /// Look up a channel by adapter name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Channel>> {
        self.channels.get(name).cloned()
    }

    /// Number of registered channels.
    #[must_use]
    pub fn len(&self) -> usize {
        self.channels.len()
    }

    /// `true` if no channels are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// Resolve the channel to use for an outbound message: prefer
    /// `msg.metadata["channel"]`, fall back to the `<name>:` prefix
    /// of `conv`.
    fn resolve(
        &self,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<Arc<dyn Channel>, ChannelError> {
        let name = msg.metadata.get("channel").map(String::as_str).or_else(|| {
            let raw = conv.as_str();
            raw.split_once(':')
                .and_then(|(name, rest)| (!name.is_empty() && !rest.is_empty()).then_some(name))
        });
        let Some(name) = name else {
            return Err(ChannelError::InvalidOwnerFormat(conv.as_str().to_owned()));
        };
        self.channels
            .get(name)
            .cloned()
            .ok_or_else(|| ChannelError::NotRegistered(name.to_owned()))
    }

    fn resolve_by_parent(
        &self,
        parent: &MessageRef,
        conv: &Owner,
    ) -> Result<Arc<dyn Channel>, ChannelError> {
        let name = if parent.channel.is_empty() {
            let raw = conv.as_str();
            raw.split_once(':')
                .and_then(|(name, rest)| (!name.is_empty() && !rest.is_empty()).then_some(name))
        } else {
            Some(parent.channel.as_str())
        };
        let Some(name) = name else {
            return Err(ChannelError::InvalidOwnerFormat(conv.as_str().to_owned()));
        };
        self.channels
            .get(name)
            .cloned()
            .ok_or_else(|| ChannelError::NotRegistered(name.to_owned()))
    }

    fn resolve_by_metadata(&self, msg: &OutboundMessage) -> Result<Arc<dyn Channel>, ChannelError> {
        let name = msg
            .metadata
            .get("channel")
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ChannelError::InvalidEnvelope(
                    "notify_user requires metadata.channel to select an adapter".to_owned(),
                )
            })?;
        self.channels
            .get(name)
            .cloned()
            .ok_or_else(|| ChannelError::NotRegistered(name.to_owned()))
    }

    fn resolve_by_conv(&self, conv: &Owner) -> Result<Arc<dyn Channel>, ChannelError> {
        let raw = conv.as_str();
        let name = raw
            .split_once(':')
            .and_then(|(name, rest)| (!name.is_empty() && !rest.is_empty()).then_some(name));
        let Some(name) = name else {
            return Err(ChannelError::InvalidOwnerFormat(conv.as_str().to_owned()));
        };
        self.channels
            .get(name)
            .cloned()
            .ok_or_else(|| ChannelError::NotRegistered(name.to_owned()))
    }
}

#[async_trait]
impl ChannelSink for ChannelRouter {
    async fn send(
        &self,
        ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let channel = self.resolve(conv, msg)?;
        channel.send(ctx, conv, msg).await
    }

    async fn react(
        &self,
        ctx: &Subject,
        conv: &Owner,
        parent: &MessageRef,
        emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        let channel = self.resolve_by_parent(parent, conv)?;
        channel.react(ctx, conv, parent, emoji).await
    }

    async fn edit(
        &self,
        ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
        new_text: &str,
    ) -> Result<(), ChannelError> {
        let channel = self.resolve_by_parent(target, conv)?;
        channel.edit(ctx, conv, target, new_text).await
    }

    async fn delete(
        &self,
        ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
    ) -> Result<(), ChannelError> {
        let channel = self.resolve_by_parent(target, conv)?;
        channel.delete(ctx, conv, target).await
    }

    async fn upload(
        &self,
        ctx: &Subject,
        conv: &Owner,
        filename: &str,
        bytes: Vec<u8>,
        comment: Option<&str>,
        thread_parent: Option<&MessageRef>,
    ) -> Result<MessageRef, ChannelError> {
        let channel = match thread_parent {
            Some(parent) => self.resolve_by_parent(parent, conv)?,
            None => self.resolve_by_conv(conv)?,
        };
        channel
            .upload(ctx, conv, filename, bytes, comment, thread_parent)
            .await
    }

    async fn read(
        &self,
        ctx: &Subject,
        conv: &Owner,
        thread_parent: Option<&MessageRef>,
        limit: usize,
    ) -> Result<Vec<ReadMessage>, ChannelError> {
        let channel = match thread_parent {
            Some(parent) => self.resolve_by_parent(parent, conv)?,
            None => self.resolve_by_conv(conv)?,
        };
        channel.read(ctx, conv, thread_parent, limit).await
    }

    async fn notify_user(
        &self,
        ctx: &Subject,
        recipient: &ParticipantId,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let channel = self.resolve_by_metadata(msg)?;
        channel.notify_user(ctx, recipient, msg).await
    }
}

#[cfg(test)]
mod router_tests;
