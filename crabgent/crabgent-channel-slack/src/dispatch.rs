//! Slack event listener registry.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_channel::{
    AudioValidator, ChannelError, ChannelInbox, ChannelKind, ChannelSubjectExt, ImageStore,
    ImageValidator, InboundEvent, KernelChannelInbox, channel_subject_id,
};
use crabgent_core::subject::{InvalidSubjectError, Subject};
use futures::future::join_all;
use secrecy::SecretString;
use tokio::task::JoinError;

use crate::error::SlackError;
use crate::events::SlackEvent;
use crate::ids::{SlackOwner, SlackWorkspaceId};
use crate::inbound::{
    ChannelKindCache, ChannelTypeCache, slack_event_to_inbound_reaction,
    slack_event_to_inbound_with_channel_type_cache,
};
use crate::subject::{SLACK_CHANNEL_ID, SLACK_THREAD_ROOT};

#[async_trait]
pub trait SlackEventListener: Send + Sync {
    async fn on_event(&self, event: SlackEvent) -> Result<(), SlackError>;
}

#[derive(Default)]
pub struct ListenerRegistry {
    listeners: Mutex<Vec<Arc<dyn SlackEventListener>>>,
}

impl ListenerRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, listener: Arc<dyn SlackEventListener>) {
        self.listeners
            .lock()
            .expect("listener registry")
            .push(listener);
    }

    pub async fn dispatch(&self, event: SlackEvent) {
        let listeners = self.listeners.lock().expect("listener registry").clone();
        tokio::task::spawn(async move {
            let handles = listeners.into_iter().map(|listener| {
                let event = event.clone();
                tokio::task::spawn(async move { listener.on_event(event).await })
            });
            for result in join_all(handles).await {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        crabgent_log::warn!(error = %error, "Slack event listener failed");
                    }
                    Err(error) => log_join_error(&error),
                }
            }
        });
    }
}

fn log_join_error(error: &JoinError) {
    crabgent_log::warn!(error = %error, "Slack event listener panicked");
}

#[must_use]
pub fn with_slack_subject_resolver(
    inbox: KernelChannelInbox,
    kind_cache: ChannelKindCache,
) -> KernelChannelInbox {
    inbox.with_fallible_subject_resolver(move |event| resolve_slack_subject(event, &kind_cache))
}

fn resolve_slack_subject(
    event: &InboundEvent,
    kind_cache: &ChannelKindCache,
) -> Result<Subject, InvalidSubjectError> {
    if event.channel.trim().is_empty() || event.from.id.as_str().trim().is_empty() {
        return Err(InvalidSubjectError);
    }
    let slack_owner: SlackOwner = event
        .conv
        .as_str()
        .parse()
        .map_err(|_err| InvalidSubjectError)?;
    let channel_id = slack_owner.channel().as_str();
    let Ok(cache) = kind_cache.lock() else {
        return Err(InvalidSubjectError);
    };
    let kind = cache.get(channel_id).copied().unwrap_or(ChannelKind::Group);
    let thread_root = event
        .message
        .thread_root
        .as_deref()
        .unwrap_or(&event.message.id);
    Ok(
        Subject::try_new(channel_subject_id(&event.channel, event.from.id.as_str()))?
            .with_participant_role(event.from.role.as_str())
            .with_channel(&event.channel, &event.conv, kind)
            .with_attr(SLACK_CHANNEL_ID, channel_id)
            .with_attr(SLACK_THREAD_ROOT, thread_root),
    )
}

/// Slack identities for the currently authenticated bot.
///
/// Slack message events identify bot-authored messages with `bot_id`,
/// while reaction events identify the acting user with `user_id`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlackSelfIds {
    bot_id: Option<String>,
    user_id: Option<String>,
}

impl SlackSelfIds {
    #[must_use]
    pub const fn new(bot_id: Option<String>, user_id: Option<String>) -> Self {
        Self { bot_id, user_id }
    }
}

pub struct KernelInboundForwarder {
    inbox: Arc<dyn ChannelInbox>,
    workspace_id: SlackWorkspaceId,
    kind_cache: ChannelKindCache,
    type_cache: ChannelTypeCache,
    self_bot_id: Option<String>,
    self_user_id: Option<String>,
    client: reqwest::Client,
    token: SecretString,
    store: Arc<dyn ImageStore>,
    validator: ImageValidator,
    audio_validator: AudioValidator,
}

impl KernelInboundForwarder {
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "forwarder owns distinct Slack dependencies wired at adapter boundary"
    )]
    pub fn new(
        inbox: Arc<dyn ChannelInbox>,
        workspace_id: SlackWorkspaceId,
        kind_cache: ChannelKindCache,
        type_cache: ChannelTypeCache,
        self_ids: SlackSelfIds,
        client: reqwest::Client,
        token: SecretString,
        store: Arc<dyn ImageStore>,
        validator: ImageValidator,
        audio_validator: AudioValidator,
    ) -> Self {
        Self {
            inbox,
            workspace_id,
            kind_cache,
            type_cache,
            self_bot_id: self_ids.bot_id,
            self_user_id: self_ids.user_id,
            client,
            token,
            store,
            validator,
            audio_validator,
        }
    }

    /// Build a forwarder with an SSRF-hardened media-download client.
    ///
    /// The media path fetches `url_private`/`url_private_download` with
    /// the bot token attached. Building the client here (no redirects,
    /// finite total `timeout`) keeps the hardening inside the adapter so
    /// the wiring boundary cannot inject a bare `reqwest::Client` that
    /// follows redirects and leaks the bearer credential to an attacker
    /// controlled host. Prefer this over [`Self::new`] for runtime wiring.
    #[expect(
        clippy::too_many_arguments,
        reason = "forwarder owns distinct Slack dependencies wired at adapter boundary"
    )]
    pub fn with_hardened_client(
        inbox: Arc<dyn ChannelInbox>,
        workspace_id: SlackWorkspaceId,
        kind_cache: ChannelKindCache,
        type_cache: ChannelTypeCache,
        self_ids: SlackSelfIds,
        download_timeout: std::time::Duration,
        token: SecretString,
        store: Arc<dyn ImageStore>,
        validator: ImageValidator,
        audio_validator: AudioValidator,
    ) -> Result<Self, SlackError> {
        let client = crate::http::build_media_client(download_timeout)?;
        Ok(Self::new(
            inbox,
            workspace_id,
            kind_cache,
            type_cache,
            self_ids,
            client,
            token,
            store,
            validator,
            audio_validator,
        ))
    }
}

#[async_trait]
impl SlackEventListener for KernelInboundForwarder {
    async fn on_event(&self, event: SlackEvent) -> Result<(), SlackError> {
        if let Some(reaction) = slack_event_to_inbound_reaction(
            &event,
            &self.workspace_id,
            &self.kind_cache,
            self.self_user_id.as_deref(),
        ) {
            return self
                .inbox
                .receive_reaction(reaction)
                .await
                .map_err(|error| channel_error(&error));
        }
        let Some(inbound) = slack_event_to_inbound_with_channel_type_cache(
            &event,
            &self.workspace_id,
            &self.kind_cache,
            &self.type_cache,
            self.self_bot_id.as_deref(),
            &self.client,
            &self.token,
            self.store.as_ref(),
            &self.validator,
            &self.audio_validator,
        )
        .await
        else {
            return Ok(());
        };
        self.inbox
            .receive(inbound)
            .await
            .map_err(|error| channel_error(&error))
    }
}

fn channel_error(error: &ChannelError) -> SlackError {
    // `PolicyDenied` renders the implementor-supplied reason verbatim and
    // `ConversationNotFound` embeds the raw conversation owner id. Both flow
    // into the ops `warn!` log via `SlackError::Internal`, so map them to
    // opaque labels (security.md Policy-Deny-Reason-Handling). Other variants
    // already render opaque or operator-safe `Display` text.
    let message = match error {
        ChannelError::PolicyDenied { .. } => "policy denied".to_owned(),
        ChannelError::ConversationNotFound(_) => "conversation not found".to_owned(),
        other => other.to_string(),
    };
    SlackError::Internal(message)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crabgent_channel::{MessageRef, Participant, ParticipantRole};
    use crabgent_core::owner::Owner;

    use super::*;
    use crate::CHANNEL_NAME;

    #[test]
    fn resolve_slack_subject_stamps_reply_routing_attrs() {
        let cache = Arc::new(Mutex::new(std::collections::HashMap::new()));
        cache
            .lock()
            .expect("channel kind cache")
            .insert("D123".to_owned(), ChannelKind::Direct);
        let event = inbound_event("slack:T123/D123", Some("1.0"));

        let subject = resolve_slack_subject(&event, &cache).expect("subject");

        assert_eq!(subject.attr(SLACK_CHANNEL_ID), Some("D123"));
        assert_eq!(subject.attr(SLACK_THREAD_ROOT), Some("1.0"));
        assert_eq!(
            subject.attr(crabgent_channel::attr_keys::CHANNEL_KIND),
            Some("direct")
        );
    }

    fn inbound_event(conv: &str, thread_root: Option<&str>) -> InboundEvent {
        let owner = Owner::new(conv);
        let message = match thread_root {
            Some(root) => MessageRef::thread_reply(CHANNEL_NAME, owner.clone(), "1.1", root),
            None => MessageRef::top_level(CHANNEL_NAME, owner.clone(), "1.1"),
        };
        InboundEvent {
            channel: CHANNEL_NAME.to_owned(),
            conv: owner,
            kind: Some(ChannelKind::Direct),
            from: Participant::new("U1", ParticipantRole::Human),
            message,
            body: "hello".to_owned(),
            attachments: vec![],
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn channel_error_redacts_policy_reason() {
        let err = ChannelError::policy_denied(
            "channel.send",
            "scope personal blocked for subject U123 token=secret-xyz",
        );
        let SlackError::Internal(message) = channel_error(&err) else {
            panic!("expected SlackError::Internal");
        };
        assert_eq!(message, "policy denied");
        assert!(!message.contains("secret-xyz"), "{message}");
        assert!(!message.contains("U123"), "{message}");
    }

    #[test]
    fn channel_error_redacts_conversation_owner() {
        let err = ChannelError::ConversationNotFound("slack:T1/C2".to_owned());
        let SlackError::Internal(message) = channel_error(&err) else {
            panic!("expected SlackError::Internal");
        };
        assert_eq!(message, "conversation not found");
        assert!(!message.contains("slack:T1/C2"), "{message}");
    }

    #[test]
    fn channel_error_preserves_safe_variant_text() {
        let err = ChannelError::NotRegistered("slack".to_owned());
        let SlackError::Internal(message) = channel_error(&err) else {
            panic!("expected SlackError::Internal");
        };
        assert!(message.contains("slack"), "{message}");
    }
}
