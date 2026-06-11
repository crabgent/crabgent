//! Slack subject helpers.

use crabgent_channel::channel_subject_id;

use crate::ids::{SlackUserId, SlackWorkspaceId};

/// Subject attr carrying the Slack conversation id for safe replies.
pub const SLACK_CHANNEL_ID: &str = "slack_channel_id";

/// Subject attr carrying the inbound Slack thread root for safe replies.
pub const SLACK_THREAD_ROOT: &str = "slack_thread_root";

/// Build a deterministic subject id for a Slack user in a workspace.
#[must_use]
pub fn slack_subject_id(workspace_id: &SlackWorkspaceId, user_id: &SlackUserId) -> String {
    channel_subject_id("slack", &format!("{workspace_id}/{user_id}"))
}
