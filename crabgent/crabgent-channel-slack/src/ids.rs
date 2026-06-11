//! Slack-specific identifiers and owner encoding.

use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use crabgent_core::owner::Owner;
use thiserror::Error;

macro_rules! slack_id {
    ($name:ident, $label:literal, $validator:ident) => {
        #[doc = $label]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Build a validated Slack identifier.
            pub fn new(value: impl Into<String>) -> Result<Self, ParseSlackIdError> {
                let value = value.into();
                if $validator(&value) {
                    Ok(Self(value))
                } else {
                    Err(ParseSlackIdError {
                        kind: stringify!($name),
                        value,
                    })
                }
            }

            /// Borrow the inner identifier.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = ParseSlackIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
    };
}

slack_id!(SlackWorkspaceId, "Slack workspace/team id.", is_upper_id);
slack_id!(
    SlackChannelId,
    "Slack channel/conversation id.",
    is_upper_id
);
slack_id!(SlackUserId, "Slack user id.", is_upper_id);
slack_id!(SlackUserGroupId, "Slack user group id.", is_group_id);
slack_id!(SlackTs, "Slack message timestamp.", is_ts);

/// Slack ID parse failure.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid {kind}: {value}")]
pub struct ParseSlackIdError {
    kind: &'static str,
    value: String,
}

/// Slack owner encoded as `slack:<workspace>/<channel>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SlackOwner {
    workspace: SlackWorkspaceId,
    channel: SlackChannelId,
}

impl SlackOwner {
    /// Build a Slack owner from typed ids.
    #[must_use]
    pub const fn new(workspace: SlackWorkspaceId, channel: SlackChannelId) -> Self {
        Self { workspace, channel }
    }

    /// Workspace id.
    #[must_use]
    pub const fn workspace(&self) -> &SlackWorkspaceId {
        &self.workspace
    }

    /// Channel id.
    #[must_use]
    pub const fn channel(&self) -> &SlackChannelId {
        &self.channel
    }

    /// Convert to crabgent core owner.
    #[must_use]
    pub fn owner(&self) -> Owner {
        Owner::new(self.to_string())
    }
}

impl Display for SlackOwner {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "slack:{}/{}", self.workspace, self.channel)
    }
}

impl FromStr for SlackOwner {
    type Err = ParseSlackOwnerError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let rest = value
            .strip_prefix("slack:")
            .ok_or(ParseSlackOwnerError::MissingPrefix)?;
        let (workspace, channel) = rest
            .split_once('/')
            .ok_or(ParseSlackOwnerError::MissingSeparator)?;
        Ok(Self {
            workspace: workspace.parse()?,
            channel: channel.parse()?,
        })
    }
}

/// Slack owner parse failure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseSlackOwnerError {
    /// Owner did not start with `slack:`.
    #[error("Slack owner must start with slack:")]
    MissingPrefix,
    /// Owner did not contain `/` between workspace and channel.
    #[error("Slack owner must use slack:<workspace>/<channel>")]
    MissingSeparator,
    /// A component was invalid.
    #[error(transparent)]
    InvalidId(#[from] ParseSlackIdError),
}

fn is_upper_id(value: &str) -> bool {
    matches!(value.len(), 2..=64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn is_group_id(value: &str) -> bool {
    matches!(value.len(), 2..=64)
        && value.starts_with('S')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn is_ts(value: &str) -> bool {
    let Some((secs, micros)) = value.split_once('.') else {
        return !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
    };
    !secs.is_empty()
        && !micros.is_empty()
        && secs.bytes().all(|byte| byte.is_ascii_digit())
        && micros.bytes().all(|byte| byte.is_ascii_digit())
}
