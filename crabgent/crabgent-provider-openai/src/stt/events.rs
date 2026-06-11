//! `OpenAI` realtime speech-to-text event parsing.

use crabgent_core::{SttError, SttEvent, SttModelId, SttResponse};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

const DELTA_EVENT: &str = "conversation.item.input_audio_transcription.delta";
const COMPLETED_EVENT: &str = "conversation.item.input_audio_transcription.completed";
const DEFAULT_REALTIME_MODEL: &str = "gpt-realtime-whisper";

pub(super) fn parse_openai_stt_event(msg: &Message) -> Option<SttEvent> {
    match msg {
        Message::Text(text) => parse_text_event(text.as_str()),
        Message::Binary(bytes) => std::str::from_utf8(bytes).ok().and_then(parse_text_event),
        Message::Close(Some(frame)) if is_auth_close(frame.code.into()) => Some(SttEvent::Error(
            SttError::Auth("openai stt authentication failed".to_owned()),
        )),
        Message::Close(_) => Some(SttEvent::Error(SttError::Network)),
        _ => None,
    }
}

const fn is_auth_close(code: u16) -> bool {
    matches!(code, 4401 | 4403)
}

fn parse_text_event(text: &str) -> Option<SttEvent> {
    let value: Value = serde_json::from_str(text).ok()?;
    match value.get("type").and_then(Value::as_str)? {
        DELTA_EVENT => Some(SttEvent::Delta(extract_text(&value).unwrap_or_default())),
        COMPLETED_EVENT => Some(SttEvent::Final(SttResponse {
            text: extract_text(&value).unwrap_or_default(),
            model: SttModelId::new(
                value
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or(DEFAULT_REALTIME_MODEL),
            ),
            segments: Vec::new(),
            audio_events: Vec::new(),
            language: None,
        })),
        "error" => Some(SttEvent::Error(SttError::Backend(
            "openai realtime transcription failed".to_owned(),
        ))),
        _ => None,
    }
}

fn extract_text(value: &Value) -> Option<String> {
    for key in ["delta", "transcript", "text"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            return Some(text.to_owned());
        }
    }
    value
        .pointer("/item/content/0/transcript")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}
