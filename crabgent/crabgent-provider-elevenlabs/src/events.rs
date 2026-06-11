//! `ElevenLabs` realtime STT event parsing.

use crabgent_core::{SttError, SttEvent, SttModelId, SttResponse};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

use crate::words::{self, RawWord, build_segments};

const PARTIAL_TRANSCRIPT: &str = "partial_transcript";
const COMMITTED_TRANSCRIPT: &str = "committed_transcript";
const COMMITTED_TRANSCRIPT_WITH_TIMESTAMPS: &str = "committed_transcript_with_timestamps";
const SESSION_STARTED: &str = "session_started";
const SCRIBE_V2_REALTIME: &str = "scribe_v2_realtime";

pub fn parse_elevenlabs_stt_event(msg: &Message) -> Option<SttEvent> {
    match msg {
        Message::Text(text) => parse_text_event(text.as_str()),
        Message::Binary(bytes) => std::str::from_utf8(bytes).ok().and_then(parse_text_event),
        Message::Close(Some(frame)) if is_auth_close(frame.code.into()) => Some(SttEvent::Error(
            SttError::Auth("elevenlabs authentication failed".to_owned()),
        )),
        Message::Close(_) => Some(SttEvent::Error(SttError::Network)),
        _ => None,
    }
}

fn parse_text_event(text: &str) -> Option<SttEvent> {
    let value: Value = serde_json::from_str(text).ok()?;
    match value.get("message_type").and_then(Value::as_str)? {
        SESSION_STARTED => None,
        PARTIAL_TRANSCRIPT => Some(SttEvent::Delta(extract_text(&value).unwrap_or_default())),
        COMMITTED_TRANSCRIPT => Some(SttEvent::Final(SttResponse {
            text: extract_text(&value).unwrap_or_default(),
            model: model_id(&value),
            segments: Vec::new(),
            audio_events: Vec::new(),
            language: None,
        })),
        COMMITTED_TRANSCRIPT_WITH_TIMESTAMPS => {
            Some(SttEvent::Final(parse_committed_with_timestamps(&value)))
        }
        "error" => Some(SttEvent::Error(SttError::Backend(
            "elevenlabs realtime transcription failed".to_owned(),
        ))),
        _ => None,
    }
}

/// Build a final `SttResponse` from a `committed_transcript_with_timestamps`
/// frame.
///
/// The frame is emitted only when the realtime connection sets
/// `include_timestamps=true`. `words[]` carries lexical `word` and `spacing`
/// entries (realtime has no inline `audio_event`), so `audio_events` stays empty
/// in practice. When timed words are present they wrap into one segment spanning
/// the transcript, mirroring the batch path; otherwise no segment is fabricated.
/// `language_code` may be absent or `null`, which degrades to `None`.
fn parse_committed_with_timestamps(value: &Value) -> SttResponse {
    let text = extract_text(value).unwrap_or_default();
    let raw_words = extract_raw_words(value);
    let (words, audio_events) = words::parse_words(&raw_words);
    let segments = build_segments(&text, words);
    let language = value
        .get("language_code")
        .and_then(Value::as_str)
        .map(str::to_owned);
    SttResponse {
        text,
        model: model_id(value),
        segments,
        audio_events,
        language,
    }
}

/// Deserialize the optional `words[]` array into `RawWord` entries.
///
/// A missing, `null`, or malformed `words` field yields an empty `Vec` rather
/// than an error: timestamps are an enrichment, never a hard requirement.
fn extract_raw_words(value: &Value) -> Vec<RawWord> {
    value
        .get("words")
        .cloned()
        .map(serde_json::from_value)
        .and_then(Result::ok)
        .unwrap_or_default()
}

fn model_id(value: &Value) -> SttModelId {
    SttModelId::new(
        value
            .get("model_id")
            .and_then(Value::as_str)
            .unwrap_or(SCRIBE_V2_REALTIME),
    )
}

fn extract_text(value: &Value) -> Option<String> {
    for key in ["text", "transcript"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            return Some(text.to_owned());
        }
    }
    None
}

const fn is_auth_close(code: u16) -> bool {
    matches!(code, 4401 | 4403)
}
