//! Slack `Channel` implementation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_channel::{
    Channel, ChannelError, ChannelKind, ConvLabel, MessageRef, OutboundMessage, Participant,
    ParticipantId, ParticipantRole, ReadMessage,
};
use crabgent_core::owner::Owner;
use crabgent_core::subject::Subject;
use futures::StreamExt;
use reqwest::Client;
use tokio::sync::Mutex;

use crate::CHANNEL_NAME;
use crate::api::{CompleteUploadFile, CompleteUploadRequest, SlackHttpClient};
use crate::channel_helpers::{
    clamp_read_limit, parse_owner, read_message_from_slack, slack_error, strip_emoji_colons,
    upload_bytes,
};
use crate::channel_names::SlackChannelNames;
use crate::ids::{SlackChannelId, SlackOwner, SlackUserId, SlackWorkspaceId};
use crate::inbound::{ChannelTypeCache, new_channel_type_cache};
use crate::outbound::{format_slack_text, outbound_to_post_message};

/// Slack adapter for outbound messages and conversation metadata.
pub struct SlackChannel {
    client: Arc<SlackHttpClient>,
    upload_http: Client,
    kind_cache: Mutex<HashMap<String, ChannelKind>>,
    type_cache: ChannelTypeCache,
    workspace_id: Option<SlackWorkspaceId>,
    channel_names: SlackChannelNames,
}

impl SlackChannel {
    #[must_use]
    pub fn new(client: Arc<SlackHttpClient>) -> Self {
        let upload_http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(client.request_timeout())
            .build()
            .expect("Slack upload HTTP client should build with request timeout");
        Self {
            client,
            upload_http,
            kind_cache: Mutex::new(HashMap::new()),
            type_cache: new_channel_type_cache(),
            workspace_id: None,
            channel_names: SlackChannelNames::default(),
        }
    }

    #[must_use]
    pub fn with_channel_type_cache(mut self, cache: ChannelTypeCache) -> Self {
        self.type_cache = cache;
        self
    }

    /// Install the pre-warmed readable channel names used by
    /// [`Channel::conv_display`].
    ///
    /// Built by [`crate::inbox::SlackInbox::pre_warm_channel_names`]. Without
    /// this, `conv_display` falls back to the empty default and the
    /// `<inbound>` tag carries no `name`/`workspace` labels.
    #[must_use]
    pub fn with_channel_names(mut self, channel_names: SlackChannelNames) -> Self {
        self.channel_names = channel_names;
        self
    }

    /// Set the workspace id (Slack team id) used to construct conv
    /// owners for `notify_user`. Without this, `notify_user` returns
    /// `ChannelError::Adapter` because the DM owner cannot be encoded
    /// in the canonical `slack:<workspace>/<channel>` shape.
    #[must_use]
    pub fn with_workspace_id(mut self, workspace: SlackWorkspaceId) -> Self {
        self.workspace_id = Some(workspace);
        self
    }

    #[must_use]
    pub fn channel_type(&self, channel_id: &str) -> Option<String> {
        self.type_cache
            .lock()
            .expect("channel type cache")
            .get(channel_id)
            .cloned()
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &'static str {
        CHANNEL_NAME
    }

    async fn kind(&self, conv: &Owner) -> Result<ChannelKind, ChannelError> {
        let owner = parse_owner(conv)?;
        let channel = owner.channel().as_str().to_owned();
        let mut kind_cache = self.kind_cache.lock().await;
        if let Some(kind) = kind_cache.get(&channel).copied() {
            return Ok(kind);
        }
        let info = self
            .client
            .conversations_info(&channel)
            .await
            .map_err(|error| slack_error(&error))?;
        let kind = if info.channel.is_im {
            ChannelKind::Direct
        } else {
            ChannelKind::Group
        };
        kind_cache.insert(channel, kind);
        Ok(kind)
    }

    /// Resolve readable labels from the pre-warmed name map.
    ///
    /// Pure local lookup, no network round-trip on the dispatch hot-path. The
    /// `name` comes from the [`SlackChannelNames`] map keyed by channel id; a
    /// DM (the channel id is an IM not present in the map) or a channel created
    /// after the pre-warm yields `name = None`, and the DM partner identity is
    /// surfaced via the sender display instead. The `workspace` is the
    /// constant-per-connection team label. A malformed owner or an entirely
    /// empty label resolves to `None` so the tag omits the attrs.
    async fn conv_display(&self, conv: &Owner) -> Option<ConvLabel> {
        let owner = parse_owner(conv).ok()?;
        let name = self
            .channel_names
            .name(owner.channel())
            .map(ToOwned::to_owned);
        let workspace = self.channel_names.workspace().map(ToOwned::to_owned);
        let label = ConvLabel { name, workspace };
        (!label.is_empty()).then_some(label)
    }

    /// Slack participant discovery is conversation-scoped. The `Subject`
    /// parameter is part of the `Channel` trait contract and is intentionally
    /// unused by this adapter implementation.
    async fn participants(
        &self,
        _ctx: &Subject,
        conv: &Owner,
    ) -> Result<Vec<Participant>, ChannelError> {
        let owner = parse_owner(conv)?;
        let channel = owner.channel().as_str();
        let members = self
            .client
            .conversations_members(channel)
            .await
            .map_err(|error| slack_error(&error))?
            .members;

        let mut seen = HashSet::with_capacity(members.len());
        let member_ids = members
            .into_iter()
            .filter(|member| seen.insert(member.clone()))
            .collect::<Vec<_>>();
        let client = Arc::clone(&self.client);
        let participants = futures::stream::iter(member_ids)
            .map(|member| {
                let client = Arc::clone(&client);
                async move {
                    match client.users_info(&member).await {
                        Ok(info) => Some(
                            Participant::new(info.user.id, ParticipantRole::Human)
                                .with_display_name(info.user.name.unwrap_or(member)),
                        ),
                        Err(error) => {
                            crabgent_log::warn!(
                                channel = %channel,
                                member = %member,
                                error = %error,
                                "Slack users.info failed while listing participants"
                            );
                            None
                        }
                    }
                }
            })
            .buffer_unordered(8)
            .filter_map(std::future::ready)
            .collect::<Vec<_>>()
            .await;
        Ok(participants)
    }

    async fn send(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let owner = parse_owner(conv)?;
        let post =
            outbound_to_post_message(owner.channel().as_str(), msg, self.client.body_cap_chars());
        let response = self
            .client
            .post_message(
                &post.channel,
                &post.text,
                post.thread_ts.as_deref(),
                post.reply_broadcast,
                post.mrkdwn,
            )
            .await
            .map_err(|error| slack_error(&error))?;
        let ts = response
            .ts
            .ok_or_else(|| ChannelError::adapter("Slack response missing ts"))?;
        let result = match post.thread_ts {
            Some(root) => MessageRef::thread_reply_broadcast(
                CHANNEL_NAME,
                conv.clone(),
                ts,
                root,
                post.reply_broadcast,
            ),
            None => MessageRef::top_level(CHANNEL_NAME, conv.clone(), ts),
        };
        Ok(result)
    }

    async fn react(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        parent: &MessageRef,
        emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        let owner = parse_owner(conv)?;
        self.client
            .reactions_add(
                owner.channel().as_str(),
                parent.id.as_str(),
                strip_emoji_colons(emoji),
            )
            .await
            .map_err(|error| slack_error(&error))?;
        Ok(MessageRef::top_level(
            CHANNEL_NAME,
            conv.clone(),
            parent.id.clone(),
        ))
    }

    async fn edit(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
        new_text: &str,
    ) -> Result<(), ChannelError> {
        let owner = parse_owner(conv)?;
        let text = format_slack_text(new_text, true);
        self.client
            .update_message(owner.channel().as_str(), &target.id, &text)
            .await
            .map_err(|error| slack_error(&error))?;
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
    ) -> Result<(), ChannelError> {
        let owner = parse_owner(conv)?;
        self.client
            .delete_message(owner.channel().as_str(), &target.id)
            .await
            .map_err(|error| slack_error(&error))?;
        Ok(())
    }

    async fn upload(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        filename: &str,
        bytes: Vec<u8>,
        comment: Option<&str>,
        thread_parent: Option<&MessageRef>,
    ) -> Result<MessageRef, ChannelError> {
        let owner = parse_owner(conv)?;
        let upload = self
            .client
            .files_get_upload_url_external(filename, bytes.len() as u64)
            .await
            .map_err(|error| slack_error(&error))?;
        upload_bytes(&self.upload_http, &upload.upload_url, bytes).await?;
        let initial_comment = comment.map(|text| format_slack_text(text, true));
        let request = CompleteUploadRequest {
            files: vec![CompleteUploadFile {
                id: &upload.file_id,
                title: filename,
            }],
            channel_id: owner.channel().as_str(),
            initial_comment: initial_comment.as_deref(),
            thread_ts: thread_parent.map(MessageRef::thread_root_or_id),
        };
        self.client
            .files_complete_upload_external(&request)
            .await
            .map_err(|error| slack_error(&error))?;
        let message_ref = match thread_parent {
            Some(parent) => MessageRef::thread_reply(
                CHANNEL_NAME,
                conv.clone(),
                upload.file_id,
                parent.thread_root_or_id(),
            ),
            None => MessageRef::top_level(CHANNEL_NAME, conv.clone(), upload.file_id),
        };
        Ok(message_ref)
    }

    async fn notify_user(
        &self,
        _ctx: &Subject,
        recipient: &ParticipantId,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let workspace = self.workspace_id.clone().ok_or_else(|| {
            ChannelError::adapter(
                "Slack notify_user requires SlackChannel::with_workspace_id to be set",
            )
        })?;
        let user = SlackUserId::new(recipient.as_str().to_owned()).map_err(|err| {
            ChannelError::InvalidEnvelope(format!("invalid Slack user id: {err}"))
        })?;
        let dm_channel_id = self
            .client
            .conversations_open(user.as_str())
            .await
            .map_err(|err| slack_error(&err))?;
        let dm_channel = SlackChannelId::new(dm_channel_id).map_err(|err| {
            ChannelError::adapter(format!("Slack DM channel id failed to validate: {err}"))
        })?;
        let conv = SlackOwner::new(workspace, dm_channel.clone()).owner();
        let post = outbound_to_post_message(dm_channel.as_str(), msg, self.client.body_cap_chars());
        let response = self
            .client
            .post_message(&post.channel, &post.text, None, false, post.mrkdwn)
            .await
            .map_err(|err| slack_error(&err))?;
        let ts = response
            .ts
            .ok_or_else(|| ChannelError::adapter("Slack response missing ts"))?;
        Ok(MessageRef::top_level(CHANNEL_NAME, conv, ts))
    }

    async fn read(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        thread_parent: Option<&MessageRef>,
        limit: usize,
    ) -> Result<Vec<ReadMessage>, ChannelError> {
        let owner = parse_owner(conv)?;
        let limit = clamp_read_limit(limit);
        let messages = if let Some(parent) = thread_parent {
            self.client
                .conversations_replies(owner.channel().as_str(), &parent.id, limit)
                .await
                .map_err(|error| slack_error(&error))?
                .messages
        } else {
            self.client
                .conversations_history(owner.channel().as_str(), limit)
                .await
                .map_err(|error| slack_error(&error))?
                .messages
        };
        Ok(messages
            .into_iter()
            .filter_map(|message| read_message_from_slack(conv, thread_parent, message))
            .collect())
    }
}
