//! Channel sink wrapper that records `notify_user` deliveries in the
//! recipient's main session.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::{
    ChannelError, ChannelSink, MessageRef, OutboundMessage, ParticipantId, ReadMessage,
};
use crabgent_core::{ContentBlock, MemoryScope, Message, Owner, Subject};
use crabgent_log::{info, warn};
use crabgent_store::SessionStore;

pub struct SessionPersistingSink {
    inner: Arc<dyn ChannelSink>,
    store: Arc<dyn SessionStore>,
    agent: String,
}

impl SessionPersistingSink {
    pub fn new(
        inner: Arc<dyn ChannelSink>,
        store: Arc<dyn SessionStore>,
        agent: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            store,
            agent: agent.into(),
        }
    }

    async fn persist_delivery(
        &self,
        ctx: &Subject,
        msg: &OutboundMessage,
        sent: &MessageRef,
        append_notify_record: bool,
    ) {
        let outbound = Message::ChannelOutbound {
            conv: sent.conv.clone(),
            body: msg.body.clone(),
            channel: sent.channel.clone(),
            message_id: sent.id.clone(),
            thread_root: sent.thread_root.clone(),
            broadcast: sent.broadcast,
        };
        let owner = target_owner(sent, &self.agent);
        let scope = target_scope(ctx, sent, &self.agent);
        let mut target = match self.store.find_or_create(&owner, None, &scope).await {
            Ok(session) => session,
            Err(err) => {
                warn!(
                    agent = %self.agent,
                    channel = %sent.channel,
                    conv = %sent.conv,
                    message_id = %sent.id,
                    error_kind = err.kind(),
                    transient = err.is_transient(),
                    "channel-persist: find_or_create failed"
                );
                return;
            }
        };
        if has_outbound(&target.messages, &sent.id) {
            return;
        }
        target.messages.push(outbound.clone());
        if append_notify_record {
            target.messages.push(notify_user_record(&outbound));
        }
        let message_count = target.messages.len();
        if let Err(err) = self
            .store
            .save_messages(&target.id, &target.messages, Utc::now())
            .await
        {
            warn!(
                agent = %self.agent,
                session_id = %target.id,
                channel = %sent.channel,
                conv = %sent.conv,
                message_id = %sent.id,
                error_kind = err.kind(),
                transient = err.is_transient(),
                "channel-persist: save failed"
            );
            return;
        }
        info!(
            agent = %self.agent,
            session_id = %target.id,
            channel = %sent.channel,
            conv = %sent.conv,
            message_id = %sent.id,
            message_count,
            "channel-persist: recorded delivery in target session"
        );
    }
}

#[async_trait]
impl ChannelSink for SessionPersistingSink {
    async fn send(
        &self,
        ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let sent = self.inner.send(ctx, conv, msg).await?;
        if should_persist_channel_send(ctx) {
            self.persist_delivery(ctx, msg, &sent, false).await;
        }
        Ok(sent)
    }

    async fn react(
        &self,
        ctx: &Subject,
        conv: &Owner,
        parent: &MessageRef,
        emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        self.inner.react(ctx, conv, parent, emoji).await
    }

    async fn edit(
        &self,
        ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
        new_text: &str,
    ) -> Result<(), ChannelError> {
        self.inner.edit(ctx, conv, target, new_text).await
    }

    async fn delete(
        &self,
        ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
    ) -> Result<(), ChannelError> {
        self.inner.delete(ctx, conv, target).await
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
        self.inner
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
        self.inner.read(ctx, conv, thread_parent, limit).await
    }

    async fn notify_user(
        &self,
        ctx: &Subject,
        recipient: &ParticipantId,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let sent = self.inner.notify_user(ctx, recipient, msg).await?;
        self.persist_delivery(ctx, msg, &sent, true).await;
        Ok(sent)
    }
}

fn should_persist_channel_send(ctx: &Subject) -> bool {
    ctx.attr("parent_task_id").is_some() || ctx.attr("cron_job_id").is_some() || ctx.id() == "cron"
}

fn target_scope(ctx: &Subject, sent: &MessageRef, agent: &str) -> MemoryScope {
    if sent.channel == crate::tui_channel::CHANNEL_NAME {
        let owner = target_owner(sent, agent);
        return MemoryScope {
            owner: Some(owner),
            channel: Some(crate::tui_channel::CHANNEL_NAME.to_owned()),
            conv: Some(sent.conv.as_str().to_owned()),
            agent: Some(tui_agent(sent).unwrap_or(agent).to_owned()),
            kind: Some("direct".to_owned()),
        };
    }
    let mut scope = MemoryScope::from_subject(ctx);
    scope.owner = Some(sent.conv.clone());
    scope.channel = Some(sent.channel.clone());
    scope.conv = Some(sent.conv.as_str().to_owned());
    scope.agent = scope.agent.or_else(|| Some(agent.to_owned()));
    scope.kind = scope.kind.or_else(|| Some("direct".to_owned()));
    scope
}

fn target_owner(sent: &MessageRef, agent: &str) -> Owner {
    if sent.channel == crate::tui_channel::CHANNEL_NAME
        && let Some(tui_agent) = tui_agent(sent)
    {
        return Owner::new(format!("tui:{tui_agent}"));
    }
    if sent.channel == crate::tui_channel::CHANNEL_NAME {
        return Owner::new(format!("tui:{agent}"));
    }
    sent.conv.clone()
}

fn tui_agent(sent: &MessageRef) -> Option<&str> {
    sent.conv
        .as_str()
        .strip_prefix("tui:")
        .and_then(|topic| topic.split('/').next())
        .filter(|agent| !agent.trim().is_empty())
}

fn has_outbound(messages: &[Message], id: &str) -> bool {
    messages
        .iter()
        .any(|msg| matches!(msg, Message::ChannelOutbound { message_id, .. } if message_id == id))
}

fn notify_user_record(outbound: &Message) -> Message {
    let Message::ChannelOutbound { body, .. } = outbound else {
        return Message::user(vec![ContentBlock::Text {
            text: "[notify_user record] Delivery metadata unavailable.".to_owned(),
        }]);
    };
    Message::user(vec![ContentBlock::Text {
        text: format!(
            "[notify_user record] You previously sent the following message into this conversation: {body}"
        ),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_store::memory::MemorySessionStore;

    #[derive(Clone)]
    struct FakeSink {
        sent: MessageRef,
    }

    #[async_trait]
    impl ChannelSink for FakeSink {
        async fn send(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
            _msg: &OutboundMessage,
        ) -> Result<MessageRef, ChannelError> {
            Ok(self.sent.clone())
        }

        async fn react(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
            _parent: &MessageRef,
            _emoji: &str,
        ) -> Result<MessageRef, ChannelError> {
            Err(ChannelError::Unsupported("react"))
        }

        async fn notify_user(
            &self,
            _ctx: &Subject,
            _recipient: &ParticipantId,
            _msg: &OutboundMessage,
        ) -> Result<MessageRef, ChannelError> {
            Ok(self.sent.clone())
        }
    }

    #[tokio::test]
    async fn notify_user_delivery_is_recorded_in_matrix_target_session() {
        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::default());
        let conv = Owner::new("matrix:!dm");
        let sent = MessageRef::top_level("matrix", conv.clone(), "$event1");
        let sink: Arc<dyn ChannelSink> = Arc::new(FakeSink { sent });
        let wrapper = SessionPersistingSink::new(sink, Arc::clone(&store), "local");
        let ctx = Subject::new("cron:mail")
            .with_attr("agent", "local")
            .with_attr("channel", "matrix")
            .with_attr("channel_kind", "direct");
        let msg = OutboundMessage::new("Mail verarbeitet").with_metadata("channel", "matrix");

        wrapper
            .notify_user(&ctx, &ParticipantId::new("@alice:example"), &msg)
            .await
            .expect("notify");

        let session = store
            .find_or_create(
                &conv,
                None,
                &MemoryScope {
                    owner: Some(conv.clone()),
                    channel: Some("matrix".to_owned()),
                    conv: Some(conv.as_str().to_owned()),
                    agent: Some("local".to_owned()),
                    kind: Some("direct".to_owned()),
                },
            )
            .await
            .expect("load session");
        assert!(matches!(
            &session.messages[0],
            Message::ChannelOutbound { message_id, body, .. }
                if message_id == "$event1" && body == "Mail verarbeitet"
        ));
        assert!(matches!(&session.messages[1], Message::User { .. }));
    }

    #[tokio::test]
    async fn notify_user_delivery_is_idempotent_by_message_id() {
        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::default());
        let conv = Owner::new("tui:local");
        let sent = MessageRef::top_level("tui", conv.clone(), "msg1");
        let sink: Arc<dyn ChannelSink> = Arc::new(FakeSink { sent });
        let wrapper = SessionPersistingSink::new(sink, Arc::clone(&store), "local");
        let ctx = Subject::new("cron:test").with_attr("agent", "local");
        let msg = OutboundMessage::new("fertig").with_metadata("channel", "tui");

        wrapper
            .notify_user(&ctx, &ParticipantId::new("local"), &msg)
            .await
            .expect("first notify");
        wrapper
            .notify_user(&ctx, &ParticipantId::new("local"), &msg)
            .await
            .expect("second notify");

        let session = store
            .find_or_create(
                &conv,
                None,
                &MemoryScope {
                    owner: Some(conv.clone()),
                    channel: Some("tui".to_owned()),
                    conv: Some(conv.as_str().to_owned()),
                    agent: Some("local".to_owned()),
                    kind: Some("direct".to_owned()),
                },
            )
            .await
            .expect("load session");
        assert_eq!(session.messages.len(), 2);
    }

    #[tokio::test]
    async fn tui_named_notify_user_delivery_uses_agent_owner_and_named_conv() {
        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::default());
        let owner = Owner::new("tui:local");
        let conv = Owner::new("tui:local/moss");
        let sent = MessageRef::top_level("tui", conv.clone(), "msg1");
        let sink: Arc<dyn ChannelSink> = Arc::new(FakeSink { sent });
        let wrapper = SessionPersistingSink::new(sink, Arc::clone(&store), "local");
        let ctx = Subject::new("tui:local")
            .with_attr("agent", "local")
            .with_attr("channel", "tui")
            .with_attr("conv", conv.as_str())
            .with_attr("channel_kind", "direct");
        let msg = OutboundMessage::new("fertig").with_metadata("channel", "tui");

        wrapper
            .notify_user(&ctx, &ParticipantId::new("local"), &msg)
            .await
            .expect("notify");

        let session = store
            .find_or_create(
                &owner,
                None,
                &MemoryScope {
                    owner: Some(owner.clone()),
                    channel: Some("tui".to_owned()),
                    conv: Some(conv.as_str().to_owned()),
                    agent: Some("local".to_owned()),
                    kind: Some("direct".to_owned()),
                },
            )
            .await
            .expect("load session");

        assert_eq!(session.owner, owner);
        assert_eq!(session.scope.conv.as_deref(), Some("tui:local/moss"));
        assert_eq!(session.messages.len(), 2);
    }

    #[tokio::test]
    async fn background_channel_send_is_recorded_in_target_session() {
        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::default());
        let conv = Owner::new("matrix:!room");
        let sent = MessageRef::top_level("matrix", conv.clone(), "$event1");
        let sink: Arc<dyn ChannelSink> = Arc::new(FakeSink { sent });
        let wrapper = SessionPersistingSink::new(sink, Arc::clone(&store), "assistant");
        let ctx = Subject::new("matrix:@alice:example")
            .with_attr("agent", "assistant")
            .with_attr("channel", "matrix")
            .with_attr("conv", conv.as_str())
            .with_attr("channel_kind", "group")
            .with_attr("parent_task_id", "task-1");
        let msg = OutboundMessage::new("Analyse fertig");

        wrapper
            .send(&ctx, &conv, &msg)
            .await
            .expect("send through wrapper");

        let session = store
            .find_or_create(
                &conv,
                None,
                &MemoryScope {
                    owner: Some(conv.clone()),
                    channel: Some("matrix".to_owned()),
                    conv: Some(conv.as_str().to_owned()),
                    agent: Some("assistant".to_owned()),
                    kind: Some("group".to_owned()),
                },
            )
            .await
            .expect("load session");
        assert_eq!(session.messages.len(), 1);
        assert!(matches!(
            &session.messages[0],
            Message::ChannelOutbound { message_id, body, .. }
                if message_id == "$event1" && body == "Analyse fertig"
        ));
    }

    #[tokio::test]
    async fn foreground_channel_send_is_not_recorded_twice() {
        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::default());
        let conv = Owner::new("matrix:!room");
        let sent = MessageRef::top_level("matrix", conv.clone(), "$event1");
        let sink: Arc<dyn ChannelSink> = Arc::new(FakeSink { sent });
        let wrapper = SessionPersistingSink::new(sink, Arc::clone(&store), "assistant");
        let ctx = Subject::new("matrix:@alice:example")
            .with_attr("agent", "assistant")
            .with_attr("channel", "matrix")
            .with_attr("conv", conv.as_str())
            .with_attr("channel_kind", "group");
        let msg = OutboundMessage::new("Live reply");

        wrapper
            .send(&ctx, &conv, &msg)
            .await
            .expect("send through wrapper");

        let session = store
            .find_or_create(
                &conv,
                None,
                &MemoryScope {
                    owner: Some(conv.clone()),
                    channel: Some("matrix".to_owned()),
                    conv: Some(conv.as_str().to_owned()),
                    agent: Some("assistant".to_owned()),
                    kind: Some("group".to_owned()),
                },
            )
            .await
            .expect("load session");
        assert!(session.messages.is_empty());
    }
}
