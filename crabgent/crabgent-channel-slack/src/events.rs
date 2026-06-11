//! Slack Socket Mode event shapes.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Socket Mode envelope received from Slack.
#[derive(Debug, Clone, Deserialize)]
pub struct SocketModeEnvelope {
    pub envelope_id: Option<String>,
    #[serde(rename = "type")]
    pub envelope_type: String,
    #[serde(default)]
    pub payload: Value,
}

impl SocketModeEnvelope {
    /// Parse the nested Events API payload, if present.
    pub fn event(&self) -> Result<Option<SlackEvent>, serde_json::Error> {
        let event_value = self
            .payload
            .get("event")
            .cloned()
            .unwrap_or_else(|| self.payload.clone());
        if event_value.is_null() {
            return Ok(None);
        }
        serde_json::from_value(event_value).map(Some)
    }
}

/// Slack Events API event.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SlackEvent {
    Message(SlackMessageEvent),
    AppMention(SlackMessageEvent),
    ReactionAdded(SlackReactionEvent),
    ReactionRemoved(SlackReactionEvent),
    MemberJoinedChannel(SlackMemberJoinedChannelEvent),
    AssistantThreadStarted(SlackAssistantThreadEvent),
    AssistantThreadContextChanged(SlackAssistantThreadEvent),
    Other { type_name: String, raw: Value },
}

impl<'de> Deserialize<'de> for SlackEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let type_name = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        match type_name.as_str() {
            "message" => decode(value, Self::Message),
            "app_mention" => decode(value, Self::AppMention),
            "reaction_added" => decode(value, Self::ReactionAdded),
            "reaction_removed" => decode(value, Self::ReactionRemoved),
            "member_joined_channel" => decode(value, Self::MemberJoinedChannel),
            "assistant_thread_started" => decode(value, Self::AssistantThreadStarted),
            "assistant_thread_context_changed" => {
                decode(value, Self::AssistantThreadContextChanged)
            }
            _ => Ok(Self::Other {
                type_name,
                raw: value,
            }),
        }
        .map_err(serde::de::Error::custom)
    }
}

fn decode<T, F>(value: Value, wrap: F) -> Result<SlackEvent, serde_json::Error>
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(T) -> SlackEvent,
{
    serde_json::from_value(value).map(wrap)
}

#[derive(Debug, Clone, Deserialize)]
pub struct SlackMessageEvent {
    pub channel: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub bot_id: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    pub ts: String,
    #[serde(default)]
    pub thread_ts: Option<String>,
    #[serde(default)]
    pub channel_type: Option<String>,
    #[serde(default, alias = "team")]
    pub team_id: Option<String>,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub files: Option<Vec<SlackFileMetadata>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SlackReactionEvent {
    pub reaction: String,
    #[serde(default)]
    pub user: Option<String>,
    pub item: SlackReactionItem,
    #[serde(default, alias = "team")]
    pub team_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SlackReactionItem {
    pub channel: String,
    pub ts: String,
}

/// Metadata for a shared Slack file.
#[derive(Debug, Clone, Deserialize)]
pub struct SlackFileMetadata {
    pub id: String,
    pub mimetype: Option<String>,
    /// Preview URL. Slack may transcode audio uploads to MP4 here.
    pub url_private: Option<String>,
    /// Original-bytes download URL. Prefer this for byte-level validation.
    #[serde(default)]
    pub url_private_download: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SlackMemberJoinedChannelEvent {
    pub channel: String,
    pub user: String,
    #[serde(default, alias = "team")]
    pub team_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SlackAssistantThreadEvent {
    #[serde(alias = "channel_id")]
    pub channel: String,
    #[serde(alias = "thread_ts")]
    pub thread_ts: String,
    #[serde(default, alias = "user_id")]
    pub user: Option<String>,
    #[serde(default, alias = "team")]
    pub team_id: Option<String>,
}
