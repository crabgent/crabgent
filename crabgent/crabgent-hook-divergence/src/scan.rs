//! Locate the latest user transcript block and extract the fields the detector
//! and the audio push need: the spoken text, the prosody signals, and the
//! retained-audio handle.
//!
//! Works on the wire-form `serde_json::Value` messages of an `LlmRequest`,
//! mirroring `crabgent_prosody::ProsodyHook` and `crabgent_tool_audio`'s hint
//! hook. By `before_llm` time the typed `ContentBlock` is already serialized.

use crabgent_core::VoiceSignals;
use serde_json::Value;

/// The pieces of one transcript block the hook acts on.
pub struct TranscriptView {
    /// Index of the user message carrying the transcript, for block insertion.
    pub message_index: usize,
    /// The transcript text (lexical input to the detector). May carry a leading
    /// `<voice>` tag if `ProsodyHook` ran first; the detector tolerates it.
    pub text: String,
    /// Prosody signals attached to the transcript. `VoiceSignals::default()`
    /// when the `voice` object is absent or unparsable (reads as neutral).
    pub voice: VoiceSignals,
    /// Retained-audio handle, when the transcript carries a non-empty one.
    pub audio_ref: Option<String>,
}

/// Find the most-recent user message that carries a transcript block and
/// extract its text, voice signals, and optional retained-audio handle.
pub fn find_latest_transcript(messages: &[Value]) -> Option<TranscriptView> {
    messages.iter().enumerate().rev().find_map(|(index, msg)| {
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            return None;
        }
        let block = msg
            .get("content")
            .and_then(Value::as_array)?
            .iter()
            .rev()
            .find(|block| block.get("type").and_then(Value::as_str) == Some("transcript"))?;
        Some(TranscriptView {
            message_index: index,
            text: block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            voice: parse_voice(block),
            audio_ref: block
                .get("source_audio")
                .and_then(Value::as_str)
                .filter(|handle| !handle.is_empty())
                .map(str::to_owned),
        })
    })
}

/// Whether the transcript at `transcript_index` belongs to the current turn.
///
/// Correlation + TTL (Hardening design 5b): the latest transcript is the current turn's
/// only when no later user message exists. A newer text-only user turn means the
/// transcript is from a prior turn, so it must not keep re-routing the audio
/// call. Assistant and tool messages after the transcript are the same turn's
/// agentic tail and do not make it stale.
pub fn is_current_turn(messages: &[Value], transcript_index: usize) -> bool {
    !messages
        .iter()
        .skip(transcript_index + 1)
        .any(|msg| msg.get("role").and_then(Value::as_str) == Some("user"))
}

/// Push a rendered block onto the content array of the message at `index`.
/// Returns whether the block was added.
pub fn push_block(messages: &mut [Value], index: usize, block: Value) -> bool {
    if let Some(content) = messages
        .get_mut(index)
        .and_then(|msg| msg.get_mut("content"))
        .and_then(Value::as_array_mut)
    {
        content.push(block);
        true
    } else {
        false
    }
}

/// Parse the `voice` object into [`VoiceSignals`], defaulting on absence or a
/// malformed value (the detector reads a default as neutral).
fn parse_voice(block: &Value) -> VoiceSignals {
    block
        .get("voice")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    fn transcript_msg(text: &str, voice: Value, audio_ref: &str) -> Value {
        let mut block = json!({"type": "transcript", "text": text, "source_audio": audio_ref});
        block["voice"] = voice;
        json!({"role": "user", "content": [block]})
    }

    #[test]
    fn finds_latest_transcript_with_fields() {
        // The voice object mirrors VoiceSignals serialization, which always
        // carries hesitation_count (no serde default on that field).
        let messages = vec![
            transcript_msg(
                "first",
                json!({"pause_ms": 100, "hesitation_count": 0}),
                "aud-old",
            ),
            json!({"role": "assistant", "content": []}),
            transcript_msg(
                "ja super",
                json!({"speech_rate_wpm": 80, "hesitation_count": 0}),
                "aud-new",
            ),
        ];
        let view = find_latest_transcript(&messages).expect("a transcript");
        assert_eq!(view.message_index, 2);
        assert_eq!(view.text, "ja super");
        assert_eq!(view.audio_ref.as_deref(), Some("aud-new"));
        assert_eq!(view.voice.speech_rate_wpm, Some(80));
    }

    #[test]
    fn missing_voice_defaults_to_neutral() {
        let messages = vec![json!({
            "role": "user",
            "content": [{"type": "transcript", "text": "hi", "source_audio": "a"}],
        })];
        let view = find_latest_transcript(&messages).expect("a transcript");
        assert_eq!(view.voice, VoiceSignals::default());
        assert_eq!(view.audio_ref.as_deref(), Some("a"));
    }

    #[test]
    fn empty_source_audio_is_none() {
        let messages = vec![transcript_msg("hi", json!({}), "")];
        let view = find_latest_transcript(&messages).expect("a transcript");
        assert_eq!(view.audio_ref, None);
    }

    #[test]
    fn no_transcript_yields_none() {
        let messages = vec![json!({
            "role": "user",
            "content": [{"type": "text", "text": "plain"}],
        })];
        assert!(find_latest_transcript(&messages).is_none());
    }

    #[test]
    fn push_block_appends_to_indexed_message() {
        let mut messages = vec![transcript_msg("hi", json!({}), "a")];
        let added = push_block(&mut messages, 0, json!({"type": "text", "text": "added"}));
        assert!(added);
        let content = messages[0]["content"].as_array().expect("content");
        assert_eq!(content.len(), 2);
        assert_eq!(content[1]["text"], "added");
    }
}
