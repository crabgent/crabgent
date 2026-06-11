//! Slack Web API request and response types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct SlackMessageResponse {
    pub ok: bool,
    pub channel: Option<String>,
    pub ts: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConversationsHistoryResponse {
    pub ok: bool,
    #[serde(default)]
    pub messages: Vec<SlackMessage>,
}

#[derive(Debug, Deserialize)]
pub struct ConversationsRepliesResponse {
    pub ok: bool,
    #[serde(default)]
    pub messages: Vec<SlackMessage>,
}

#[derive(Debug, Deserialize)]
pub struct ConversationInfo {
    pub ok: bool,
    pub channel: SlackConversation,
}

#[derive(Debug, Deserialize)]
pub struct ConversationsMembersResponse {
    pub ok: bool,
    #[serde(default)]
    pub members: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SlackUserInfo {
    pub ok: bool,
    pub user: SlackUser,
}

#[derive(Debug, Deserialize)]
pub struct UsersConversationsResponse {
    pub ok: bool,
    #[serde(default)]
    pub channels: Vec<SlackConversation>,
}

#[derive(Debug, Deserialize)]
pub struct ConversationsListResponse {
    pub ok: bool,
    #[serde(default)]
    pub channels: Vec<SlackConversation>,
    #[serde(default)]
    pub response_metadata: ResponseMetadata,
}

/// Slack cursor-pagination envelope. `next_cursor` is empty on the last page.
#[derive(Debug, Default, Deserialize)]
pub struct ResponseMetadata {
    #[serde(default)]
    pub next_cursor: String,
}

#[derive(Debug, Deserialize)]
pub struct UserGroupUsersListResponse {
    pub ok: bool,
    #[serde(default)]
    pub users: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReactionResponse {
    pub ok: bool,
}

#[derive(Debug, Deserialize)]
pub struct FilesGetUploadUrlResponse {
    pub ok: bool,
    pub upload_url: String,
    pub file_id: String,
}

#[derive(Debug, Deserialize)]
pub struct FilesCompleteUploadResponse {
    pub ok: bool,
}

#[derive(Debug, Deserialize)]
pub struct SearchMessagesResponse {
    pub ok: bool,
    pub messages: SearchMessages,
}

#[derive(Debug, Deserialize)]
pub struct AppsConnectionsOpenResponse {
    pub ok: bool,
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct ConversationsOpenResponse {
    pub ok: bool,
    pub channel: Option<SlackConversation>,
}

#[derive(Debug, Deserialize)]
pub struct AuthTestResponse {
    pub ok: bool,
    pub error: Option<String>,
    pub user_id: Option<String>,
    pub bot_id: Option<String>,
    pub team: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SlackMessage {
    pub ts: Option<String>,
    pub text: Option<String>,
    pub user: Option<String>,
    pub bot_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Slack conversation responses expose these independent API flags flat"
)]
pub struct SlackConversation {
    pub id: String,
    /// Human-readable channel name (e.g. `platform-ops`). Absent for IM
    /// conversations and when the Slack response omits the field.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub is_private: bool,
    #[serde(default)]
    pub is_member: bool,
    #[serde(default)]
    pub is_im: bool,
    #[serde(default)]
    pub is_mpim: bool,
}

#[derive(Debug, Deserialize)]
pub struct SlackUser {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchMessages {
    #[serde(default)]
    pub matches: Vec<SearchMessageMatch>,
}

#[derive(Debug, Deserialize)]
pub struct SearchMessageMatch {
    pub text: Option<String>,
    pub username: Option<String>,
    pub ts: Option<String>,
    pub permalink: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PostMessageRequest<'a> {
    pub(super) channel: &'a str,
    pub(super) text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) thread_ts: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reply_broadcast: Option<bool>,
    pub(super) mrkdwn: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct UpdateMessageRequest<'a> {
    pub(super) channel: &'a str,
    pub(super) ts: &'a str,
    pub(super) text: &'a str,
}

#[derive(Debug, Serialize)]
pub(super) struct TimestampRequest<'a> {
    pub(super) channel: &'a str,
    pub(super) ts: &'a str,
}

#[derive(Debug, Serialize)]
pub(super) struct HistoryRequest<'a> {
    pub(super) channel: &'a str,
    pub(super) limit: u32,
}

#[derive(Debug, Serialize)]
pub(super) struct RepliesRequest<'a> {
    pub(super) channel: &'a str,
    pub(super) ts: &'a str,
    pub(super) limit: u32,
}

#[derive(Debug, Serialize)]
pub(super) struct ChannelRequest<'a> {
    pub(super) channel: &'a str,
}

#[derive(Debug, Serialize)]
pub(super) struct UserRequest<'a> {
    pub(super) user: &'a str,
}

#[derive(Debug, Serialize)]
pub(super) struct UserConversationsRequest<'a> {
    pub(super) user: &'a str,
    pub(super) types: String,
}

#[derive(Debug, Serialize)]
pub(super) struct ConversationsListRequest<'a> {
    pub(super) types: String,
    pub(super) exclude_archived: bool,
    pub(super) limit: u32,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub(super) cursor: &'a str,
}

#[derive(Debug, Serialize)]
pub(super) struct UserGroupRequest<'a> {
    pub(super) usergroup: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationType {
    PublicChannel,
    PrivateChannel,
    Mpim,
    Im,
}

impl ConversationType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PublicChannel => "public_channel",
            Self::PrivateChannel => "private_channel",
            Self::Mpim => "mpim",
            Self::Im => "im",
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct ReactionRequest<'a> {
    pub(super) channel: &'a str,
    pub(super) timestamp: &'a str,
    pub(super) name: &'a str,
}

#[derive(Debug, Serialize)]
pub(super) struct UploadUrlRequest<'a> {
    pub(super) filename: &'a str,
    pub(super) length: u64,
}

/// Request body for `files.completeUploadExternal`.
#[derive(Debug, Serialize)]
pub struct CompleteUploadRequest<'a> {
    pub files: Vec<CompleteUploadFile<'a>>,
    pub channel_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_comment: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_ts: Option<&'a str>,
}

/// File entry for `files.completeUploadExternal`.
#[derive(Debug, Serialize)]
pub struct CompleteUploadFile<'a> {
    pub id: &'a str,
    pub title: &'a str,
}

#[derive(Debug, Serialize)]
pub(super) struct SearchRequest<'a> {
    pub(super) query: &'a str,
    pub(super) count: u32,
}

#[derive(Debug, Serialize)]
pub(super) struct EmptyRequest {}

#[derive(Debug, Serialize)]
pub(super) struct ConversationsOpenRequest<'a> {
    pub(super) users: &'a str,
}
