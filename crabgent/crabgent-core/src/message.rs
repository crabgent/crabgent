//! Conversation message types.
//!
//! Public API: typed `Message` enum. Loop-internal storage: `RawMessages`
//! (loose `serde_json::Value` list). Conversion via `From` / `TryFrom`.

use chrono::{DateTime, Utc};
use serde::de::Error as _;
use serde::ser::SerializeStruct as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::owner::Owner;
use crate::types::ToolCall;
use crate::voice::{AudioRef, VoiceSignals};

mod payload;
pub mod tail;

pub use payload::{
    AUDIO_PAYLOAD_ALLOWED_MIMES, AudioPayload, FilePayload, ImagePayload, PayloadError,
};

/// Maximum decoded size for JSON-transported image payload bytes.
pub const IMAGE_PAYLOAD_MAX_BYTES: usize = 5_000_000;

/// Maximum decoded size for JSON-transported audio payload bytes.
pub const AUDIO_PAYLOAD_MAX_BYTES: usize = 25_000_000;

/// Maximum decoded size for JSON-transported file payload bytes.
pub const FILE_PAYLOAD_MAX_BYTES: usize = 25_000_000;

/// A conversation message in the kernel's typed public API.
///
/// Serializes with an explicit `role` tag and `snake_case` variant names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Message {
    /// System prompt (typically only at the start, but providers vary).
    System { content: String },
    /// User-authored message.
    User {
        content: Vec<ContentBlock>,
        /// When the user authored the message. Channel adapters set this
        /// from platform timestamps (Slack `ts`, Matrix `origin_server_ts`,
        /// Telegram `date`); other call sites use `None` until a clock
        /// source is wired. Time-aware hooks read it via JSON to compute
        /// deltas and pause-markers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    /// Assistant turn output.
    Assistant {
        text: String,
        tool_calls: Vec<ToolCall>,
    },
    /// Result of a tool call, sent back into the conversation.
    ToolResult {
        call_id: String,
        output: Value,
        is_error: bool,
    },
    /// Outbound message delivered via a channel (e.g. Slack, Matrix).
    /// Fields are inline primitives to avoid cross-crate type dependencies
    /// from channel crates into crabgent-core.
    ChannelOutbound {
        conv: Owner,
        body: String,
        channel: String,
        message_id: String,
        thread_root: Option<String>,
        broadcast: bool,
    },
    /// Raw provider block from a server-side tool (e.g. hosted web search).
    ///
    /// Providers emit opaque block objects that must be echoed back in
    /// subsequent turns so the provider can correlate its own tool results.
    /// The `provider` field identifies the originating provider by name
    /// (`"anthropic"`, `"openai"`, `"google"`). `block` is the verbatim
    /// provider JSON, preserved for multi-turn echo without re-encoding.
    ProviderBlock { provider: String, block: Value },
}

impl Message {
    /// User message without a known authoring time.
    #[must_use]
    pub const fn user(content: Vec<ContentBlock>) -> Self {
        Self::User {
            content,
            timestamp: None,
        }
    }

    /// User message stamped with the channel-provided authoring time.
    #[must_use]
    pub const fn user_at(content: Vec<ContentBlock>, timestamp: DateTime<Utc>) -> Self {
        Self::User {
            content,
            timestamp: Some(timestamp),
        }
    }
}

/// A content block within a user message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContentBlock {
    /// Plain text.
    Text { text: String },
    /// Image content in provider-neutral form.
    Image(ImagePayload),
    /// Audio content in provider-neutral form.
    Audio(AudioPayload),
    /// Generic file content in provider-neutral form.
    File(FilePayload),
    /// Transcribed audio: the recognized text plus a reference to the
    /// retained source bytes and derived voice-perception signals.
    ///
    /// Emitted by the STT inbox when an `AudioStore` is wired. The text
    /// reaches text-only chat models, while `source_audio` lets an
    /// audio-capable side channel re-listen to the original bytes.
    /// `voice` is filled by the prosody pipeline when provider metadata is
    /// available.
    Transcript {
        text: String,
        source_audio: AudioRef,
        voice: Option<VoiceSignals>,
    },
}

impl Serialize for ContentBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Text { text } => {
                let mut state = serializer.serialize_struct("ContentBlock", 2)?;
                state.serialize_field("type", "text")?;
                state.serialize_field("text", text)?;
                state.end()
            }
            Self::Image(payload) => {
                let mut state = serializer.serialize_struct("ContentBlock", 3)?;
                state.serialize_field("type", "image")?;
                state.serialize_field("mime", payload.mime())?;
                state.serialize_field("data", &payload.encoded_data())?;
                state.end()
            }
            Self::Audio(payload) => {
                let mut state = serializer.serialize_struct("ContentBlock", 4)?;
                state.serialize_field("type", "audio")?;
                state.serialize_field("mime", payload.mime())?;
                state.serialize_field("data", &payload.encoded_data())?;
                state.serialize_field("filename", &payload.filename)?;
                state.end()
            }
            Self::File(payload) => {
                let mut state = serializer.serialize_struct("ContentBlock", 4)?;
                state.serialize_field("type", "file")?;
                state.serialize_field("mime", payload.mime())?;
                state.serialize_field("data", &payload.encoded_data())?;
                state.serialize_field("filename", &payload.filename)?;
                state.end()
            }
            Self::Transcript {
                text,
                source_audio,
                voice,
            } => {
                let mut state = serializer.serialize_struct("ContentBlock", 4)?;
                state.serialize_field("type", "transcript")?;
                state.serialize_field("text", text)?;
                state.serialize_field("source_audio", source_audio)?;
                // `voice` is always emitted (as `null` when absent) so the
                // key is present for readers; deserialize tolerates it
                // missing via `#[serde(default)]`.
                state.serialize_field("voice", voice)?;
                state.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ContentBlockKind {
            #[serde(rename = "type")]
            kind: String,
        }

        #[derive(Deserialize)]
        struct TextBlockWire {
            text: String,
        }

        let value = Value::deserialize(deserializer)?;
        let kind = ContentBlockKind::deserialize(&value)
            .map_err(D::Error::custom)?
            .kind;

        match kind.as_str() {
            "text" => {
                let wire = TextBlockWire::deserialize(value).map_err(D::Error::custom)?;
                Ok(Self::Text { text: wire.text })
            }
            "image" => ImagePayload::deserialize(value)
                .map(Self::Image)
                .map_err(D::Error::custom),
            "audio" => AudioPayload::deserialize(value)
                .map(Self::Audio)
                .map_err(D::Error::custom),
            "file" => FilePayload::deserialize(value)
                .map(Self::File)
                .map_err(D::Error::custom),
            "transcript" => {
                #[derive(Deserialize)]
                struct TranscriptWire {
                    text: String,
                    source_audio: AudioRef,
                    #[serde(default)]
                    voice: Option<VoiceSignals>,
                }
                let wire = TranscriptWire::deserialize(value).map_err(D::Error::custom)?;
                Ok(Self::Transcript {
                    text: wire.text,
                    source_audio: wire.source_audio,
                    voice: wire.voice,
                })
            }
            other => Err(D::Error::custom(format!(
                "unknown content block type '{other}'"
            ))),
        }
    }
}

/// Loose JSON representation used internally by the loop.
///
/// The loop driver stores messages in this form so hooks and providers
/// can rewrite them without typing constraints. Conversion via `From`
/// (typed -> raw, infallible) and `TryFrom` (raw -> typed, fallible).
#[derive(Debug, Clone, Default)]
pub struct RawMessages(pub Vec<Value>);

impl RawMessages {
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn push(&mut self, value: Value) {
        self.0.push(value);
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<Value> {
        self.0
    }

    #[must_use]
    pub fn as_slice(&self) -> &[Value] {
        &self.0
    }
}

impl From<Vec<Message>> for RawMessages {
    #[expect(
        clippy::expect_used,
        reason = "Message serialization has no fallible map keys or custom serializer failures"
    )]
    fn from(messages: Vec<Message>) -> Self {
        let raw = messages
            .into_iter()
            .map(|m| serde_json::to_value(m).expect("Message always serialisable"))
            .collect();
        Self(raw)
    }
}

impl TryFrom<RawMessages> for Vec<Message> {
    type Error = serde_json::Error;
    fn try_from(raw: RawMessages) -> Result<Self, Self::Error> {
        raw.0.into_iter().map(serde_json::from_value).collect()
    }
}

#[cfg(test)]
mod tests;
