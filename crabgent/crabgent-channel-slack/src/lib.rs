//! Slack channel adapter for crabgent.
//!
//! B2 provides injected config, typed IDs, subject helpers, and the
//! Slack Web API surface. Socket Mode, tools, and the channel
//! implementation are added in later bullets.

/// Alias keeps `#[crabgent_log::instrument]` proc-macro expansion (which emits
/// `::tracing::*` paths) resolving to `crabgent_log` without re-introducing a
/// direct `tracing` dep.
extern crate crabgent_log as tracing;

pub mod agent_progress;
pub mod api;
pub mod block_kit;
pub mod channel;
mod channel_helpers;
pub mod channel_names;
pub mod config;
pub mod connection;
pub mod dispatch;
pub mod error;
pub mod events;
pub mod files_info;
pub mod formatting;
pub mod http;
pub mod ids;
pub mod image_download;
pub mod inbound;
pub mod inbox;
pub mod outbound;
pub mod socket_mode;
#[cfg(any(test, feature = "test-support", debug_assertions))]
pub mod socket_mode_mock;
pub mod subject;
pub mod tools;
pub mod typing;

pub use agent_progress::{
    AGENT_STATUS_HEARTBEAT_INTERVAL, AgentProgressConfig, AgentProgressError, AgentProgressResult,
    DEFAULT_IDLE_FLUSH_INTERVAL, DEFAULT_SILENT_TOOLS, NoopSlackAgentProgress, ProgressChunk,
    SENTINEL_NOT_AGENT_ERRORS, SlackAgentProgress, SlackAgentProgressHook,
    SlackAgentProgressIndicator, SlackAppType,
};
pub use api::{
    AppsConnectionsOpenResponse, CompleteUploadFile, CompleteUploadRequest, ConversationInfo,
    ConversationType, ConversationsHistoryResponse, ConversationsListResponse,
    ConversationsMembersResponse, ConversationsRepliesResponse, FilesCompleteUploadResponse,
    FilesGetUploadUrlResponse, ReactionResponse, ResponseMetadata, SearchMessagesResponse,
    SlackConversation, SlackHttpClient, SlackMessageResponse, SlackUserInfo,
};
pub use block_kit::{
    BlocksChunk, MarkdownTextChunk, PlanUpdateChunk, StreamChunk, StreamHandle, TaskSource,
    TaskStatus, TaskUpdateChunk,
};
pub use channel::SlackChannel;
pub use channel_names::SlackChannelNames;
pub use config::SlackConfig;
pub use error::SlackError;
pub use formatting::SLACK_FORMATTING_HINT;
pub use ids::{
    ParseSlackIdError, ParseSlackOwnerError, SlackChannelId, SlackOwner, SlackTs, SlackUserGroupId,
    SlackUserId, SlackWorkspaceId,
};
pub use inbox::SlackInbox;
pub use subject::slack_subject_id;
pub use typing::SlackTypingIndicator;

/// Canonical channel name used by Slack message references.
pub const CHANNEL_NAME: &str = "slack";
