//! Slack Web API client.

use std::time::Duration;

use reqwest::Client;
use secrecy::SecretString;
use serde::Serialize;

use crate::config::SlackConfig;
use crate::error::SlackError;
use crate::http;

mod agent_progress;
mod retry;
mod types;

use crate::ids::{SlackChannelId, SlackUserGroupId, SlackUserId};
pub use types::{
    AppsConnectionsOpenResponse, AuthTestResponse, CompleteUploadFile, CompleteUploadRequest,
    ConversationInfo, ConversationType, ConversationsHistoryResponse, ConversationsListResponse,
    ConversationsMembersResponse, ConversationsOpenResponse, ConversationsRepliesResponse,
    FilesCompleteUploadResponse, FilesGetUploadUrlResponse, ReactionResponse, ResponseMetadata,
    SearchMessageMatch, SearchMessages, SearchMessagesResponse, SlackConversation, SlackMessage,
    SlackMessageResponse, SlackUser, SlackUserInfo, UserGroupUsersListResponse,
    UsersConversationsResponse,
};

use types::{
    ChannelRequest, ConversationsListRequest, ConversationsOpenRequest, EmptyRequest,
    HistoryRequest, PostMessageRequest, ReactionRequest, RepliesRequest, SearchRequest,
    TimestampRequest, UpdateMessageRequest, UploadUrlRequest, UserConversationsRequest,
    UserGroupRequest, UserRequest,
};

/// Per-page channel count requested from `conversations.list`. Slack caps
/// this at 1000 and recommends staying well under it; 200 keeps each page
/// response small while bounding the round-trip count for typical workspaces.
const CONVERSATIONS_LIST_PAGE_LIMIT: u32 = 200;

/// Hard ceiling on `conversations.list` pages followed in one pre-warm, so a
/// server that never clears `next_cursor` cannot spin the loop indefinitely.
const CONVERSATIONS_LIST_MAX_PAGES: u32 = 100;

/// Slack Web API client backed by injected config.
#[derive(Clone)]
pub struct SlackHttpClient {
    config: SlackConfig,
    http: Client,
}

impl SlackHttpClient {
    /// Build a Slack API client.
    pub fn new(config: SlackConfig) -> Result<Self, SlackError> {
        config.validate()?;
        let http = http::build_client(config.request_timeout)?;
        Ok(Self { config, http })
    }

    /// Build a client with an explicit reqwest client.
    #[must_use]
    pub const fn from_http(config: SlackConfig, http: Client) -> Self {
        Self { config, http }
    }

    /// Return the configured per-request timeout.
    #[must_use]
    pub const fn request_timeout(&self) -> Duration {
        self.config.request_timeout
    }

    /// Return the configured outbound body cap.
    #[must_use]
    pub const fn body_cap_chars(&self) -> usize {
        self.config.body_cap_chars
    }

    /// Call `chat.postMessage`.
    #[crabgent_log::instrument(skip(self, text))]
    pub async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
        reply_broadcast: bool,
        mrkdwn: bool,
    ) -> Result<SlackMessageResponse, SlackError> {
        let body = PostMessageRequest {
            channel,
            text,
            thread_ts,
            reply_broadcast: reply_broadcast.then_some(true),
            mrkdwn,
        };
        // Slack 429 means pre-processing rejection, so retrying this POST is safe.
        self.retry_json("chat.postMessage", &body, self.bot_token())
            .await
    }

    /// Call `chat.update`.
    #[crabgent_log::instrument(skip(self, text))]
    pub async fn update_message(
        &self,
        channel: &str,
        ts: &str,
        text: &str,
    ) -> Result<SlackMessageResponse, SlackError> {
        let body = UpdateMessageRequest { channel, ts, text };
        self.retry_json("chat.update", &body, self.bot_token())
            .await
    }

    /// Call `chat.delete`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn delete_message(
        &self,
        channel: &str,
        ts: &str,
    ) -> Result<SlackMessageResponse, SlackError> {
        let body = TimestampRequest { channel, ts };
        self.retry_form("chat.delete", &body, self.bot_token())
            .await
    }

    /// Call `conversations.history`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn conversations_history(
        &self,
        channel: &str,
        limit: u32,
    ) -> Result<ConversationsHistoryResponse, SlackError> {
        let body = HistoryRequest { channel, limit };
        self.retry_form("conversations.history", &body, self.bot_token())
            .await
    }

    /// Call `conversations.replies`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn conversations_replies(
        &self,
        channel: &str,
        ts: &str,
        limit: u32,
    ) -> Result<ConversationsRepliesResponse, SlackError> {
        let body = RepliesRequest { channel, ts, limit };
        self.retry_form("conversations.replies", &body, self.bot_token())
            .await
    }

    /// Call `conversations.info`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn conversations_info(&self, channel: &str) -> Result<ConversationInfo, SlackError> {
        let body = ChannelRequest { channel };
        self.retry_form("conversations.info", &body, self.bot_token())
            .await
    }

    /// Call `conversations.members`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn conversations_members(
        &self,
        channel: &str,
    ) -> Result<ConversationsMembersResponse, SlackError> {
        let body = ChannelRequest { channel };
        self.retry_form("conversations.members", &body, self.bot_token())
            .await
    }

    /// Call `users.info`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn users_info(&self, user: &str) -> Result<SlackUserInfo, SlackError> {
        let body = UserRequest { user };
        self.retry_form("users.info", &body, self.bot_token()).await
    }

    /// Call `users.conversations`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn users_conversations(
        &self,
        user: &SlackUserId,
        types: &[ConversationType],
    ) -> Result<Vec<SlackChannelId>, SlackError> {
        let body = UserConversationsRequest {
            user: user.as_str(),
            types: join_conversation_types(types),
        };
        let response: UsersConversationsResponse = self
            .retry_form("users.conversations", &body, self.bot_token())
            .await?;
        response
            .channels
            .into_iter()
            .map(|channel| SlackChannelId::new(channel.id))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| SlackError::Internal(error.to_string()))
    }

    /// Call `conversations.list`, following cursor pagination until the
    /// final page, and return the joined channel list.
    ///
    /// `types` selects the conversation kinds to list (the pre-warm caller
    /// passes public/private channels and IMs); archived conversations are
    /// excluded. Each page is capped at `CONVERSATIONS_LIST_PAGE_LIMIT`;
    /// the loop is bounded by `CONVERSATIONS_LIST_MAX_PAGES` so a buggy
    /// server that never clears `next_cursor` cannot spin forever.
    #[crabgent_log::instrument(skip(self))]
    pub async fn conversations_list(
        &self,
        types: &[ConversationType],
    ) -> Result<Vec<SlackConversation>, SlackError> {
        let joined_types = join_conversation_types(types);
        let mut channels = Vec::new();
        let mut cursor = String::new();
        for _page in 0..CONVERSATIONS_LIST_MAX_PAGES {
            let page = self.conversations_list_page(&joined_types, &cursor).await?;
            channels.extend(page.channels);
            cursor = page.response_metadata.next_cursor;
            if cursor.is_empty() {
                break;
            }
        }
        if !cursor.is_empty() {
            // The page cap was hit before Slack cleared next_cursor: the
            // returned list is truncated. Stay fail-soft (return what we have)
            // but make the truncation visible so the caller is not silently
            // working off a partial pre-warm set.
            crabgent_log::warn!(
                count = channels.len(),
                "conversations.list pre-warm truncated at page cap"
            );
        }
        Ok(channels)
    }

    async fn conversations_list_page(
        &self,
        joined_types: &str,
        cursor: &str,
    ) -> Result<ConversationsListResponse, SlackError> {
        let body = ConversationsListRequest {
            types: joined_types.to_owned(),
            exclude_archived: true,
            limit: CONVERSATIONS_LIST_PAGE_LIMIT,
            cursor,
        };
        self.retry_form("conversations.list", &body, self.bot_token())
            .await
    }

    /// Call `usergroups.users.list`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn usergroups_users_list(
        &self,
        group_id: &SlackUserGroupId,
    ) -> Result<Vec<SlackUserId>, SlackError> {
        let body = UserGroupRequest {
            usergroup: group_id.as_str(),
        };
        let response: UserGroupUsersListResponse = self
            .retry_form("usergroups.users.list", &body, self.bot_token())
            .await?;
        response
            .users
            .into_iter()
            .map(SlackUserId::new)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| SlackError::Internal(error.to_string()))
    }

    /// Call `reactions.add`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn reactions_add(
        &self,
        channel: &str,
        timestamp: &str,
        name: &str,
    ) -> Result<ReactionResponse, SlackError> {
        let body = ReactionRequest {
            channel,
            timestamp,
            name,
        };
        self.retry_form("reactions.add", &body, self.bot_token())
            .await
    }

    /// Call `reactions.remove`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn reactions_remove(
        &self,
        channel: &str,
        timestamp: &str,
        name: &str,
    ) -> Result<ReactionResponse, SlackError> {
        let body = ReactionRequest {
            channel,
            timestamp,
            name,
        };
        self.retry_form("reactions.remove", &body, self.bot_token())
            .await
    }

    /// Call `files.getUploadURLExternal`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn files_get_upload_url_external(
        &self,
        filename: &str,
        length: u64,
    ) -> Result<FilesGetUploadUrlResponse, SlackError> {
        let body = UploadUrlRequest { filename, length };
        self.retry_form("files.getUploadURLExternal", &body, self.bot_token())
            .await
    }

    /// Call `files.completeUploadExternal`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn files_complete_upload_external(
        &self,
        request: &CompleteUploadRequest<'_>,
    ) -> Result<FilesCompleteUploadResponse, SlackError> {
        self.retry_json("files.completeUploadExternal", request, self.bot_token())
            .await
    }

    /// Call `search.messages`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn search_messages(
        &self,
        query: &str,
        count: u32,
    ) -> Result<SearchMessagesResponse, SlackError> {
        let body = SearchRequest { query, count };
        self.retry_form("search.messages", &body, self.bot_token())
            .await
    }

    /// Call `apps.connections.open`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn apps_connections_open(&self) -> Result<AppsConnectionsOpenResponse, SlackError> {
        self.post_form("apps.connections.open", &EmptyRequest {}, self.app_token())
            .await
    }

    /// Call `conversations.open` to fetch or create the IM channel
    /// shared with `user`. Returns the DM channel id (`D...`).
    ///
    /// `conversations.open` is idempotent: Slack returns the existing
    /// IM channel id when one is already open, and creates a fresh
    /// one otherwise. `ok=false` responses are intercepted by
    /// `decode_slack_response` and surfaced as `SlackError::ApiError`
    /// carrying Slack's error code (e.g. `user_not_found`); reaching
    /// the `channel.is_none` guard requires Slack to return
    /// `ok:true, channel:null`, which it does not under normal
    /// operation but the guard stays as a defensive backstop.
    #[crabgent_log::instrument(skip(self))]
    pub async fn conversations_open(&self, user: &str) -> Result<String, SlackError> {
        let body = ConversationsOpenRequest { users: user };
        let response: ConversationsOpenResponse = self
            .retry_form("conversations.open", &body, self.bot_token())
            .await?;
        let channel = response.channel.ok_or_else(|| {
            SlackError::Internal(
                "conversations.open returned ok:true without a channel object".to_owned(),
            )
        })?;
        Ok(channel.id)
    }

    /// Call `auth.test` to identify the bot user.
    ///
    /// Returns `bot_id` and `user_id` values which can be used to filter
    /// the bot's own message echoes and reaction echoes. On failure,
    /// consumers should log a warning and proceed with both values unset
    /// (fail-open: own echoes are not filtered).
    #[crabgent_log::instrument(skip(self))]
    pub async fn auth_test(&self) -> Result<AuthTestResponse, SlackError> {
        self.retry_form("auth.test", &EmptyRequest {}, self.bot_token())
            .await
    }

    pub(super) async fn retry_form<T, B>(
        &self,
        method: &str,
        body: &B,
        token: &SecretString,
    ) -> Result<T, SlackError>
    where
        T: serde::de::DeserializeOwned + Send,
        B: Serialize + Sync,
    {
        retry::rate_limited(self.config.retry_max, || {
            self.post_form(method, body, token)
        })
        .await
    }

    pub(super) async fn retry_json<T, B>(
        &self,
        method: &str,
        body: &B,
        token: &SecretString,
    ) -> Result<T, SlackError>
    where
        T: serde::de::DeserializeOwned + Send,
        B: Serialize + Sync,
    {
        retry::rate_limited(self.config.retry_max, || {
            self.post_json(method, body, token)
        })
        .await
    }

    async fn post_json<T, B>(
        &self,
        method: &str,
        body: &B,
        token: &SecretString,
    ) -> Result<T, SlackError>
    where
        T: serde::de::DeserializeOwned + Send,
        B: Serialize + Sync,
    {
        http::send_json(&self.http, token, &self.url(method), body).await
    }

    async fn post_form<T, B>(
        &self,
        method: &str,
        body: &B,
        token: &SecretString,
    ) -> Result<T, SlackError>
    where
        T: serde::de::DeserializeOwned + Send,
        B: Serialize + Sync,
    {
        http::send_form(&self.http, token, &self.url(method), body).await
    }

    pub(super) fn url(&self, method: &str) -> String {
        format!(
            "{}/{}",
            self.config.api_base().trim_end_matches('/'),
            method
        )
    }

    pub(super) const fn bot_token(&self) -> &SecretString {
        &self.config.bot_token
    }

    const fn app_token(&self) -> &SecretString {
        &self.config.app_token
    }
}

fn join_conversation_types(types: &[ConversationType]) -> String {
    types
        .iter()
        .map(|ty| ty.as_str())
        .collect::<Vec<_>>()
        .join(",")
}
