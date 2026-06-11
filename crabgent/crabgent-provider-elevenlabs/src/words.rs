//! `ElevenLabs` word-level transcription parsing.
//!
//! The batch and realtime transports share the same `words[]` payload shape:
//! lexical words, spacing tokens, and inline audio events. `parse_words` maps
//! that wire shape onto the neutral `SttWord` / `AudioEvent` core types.

use crabgent_core::{AudioEvent, SttSegment, SttWord};
use serde::Deserialize;

/// One `words[]` entry from an `ElevenLabs` speech-to-text response.
///
/// `start`/`end` are deserialized directly into `f32` (seconds) to avoid an
/// `f64 -> f32` cast and the `cast_precision_loss` lint. They are `null` for
/// some entry kinds, so both are optional. `speaker_id` is present when
/// diarization is enabled. `logprob` is intentionally not declared because
/// it does not map to core's `[0, 1]` confidence field.
#[derive(Debug, Deserialize)]
pub struct RawWord {
    text: String,
    #[serde(default)]
    start: Option<f32>,
    #[serde(default)]
    end: Option<f32>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    speaker_id: Option<String>,
}

/// Split `ElevenLabs` `words[]` entries into lexical words and audio events.
///
/// - `type == "word"`: pushed as `SttWord` only when both `start` and `end` are
///   present. Words with missing timing are skipped here; their text already
///   lives in `SttResponse.text`. No fabricated zero timestamps.
/// - `type == "audio_event"`: pushed as `AudioEvent` with the `text` as label
///   and optional millisecond offsets (null -> `None`, no fabrication).
/// - `type == "spacing"` or absent: skipped.
pub fn parse_words(raw: &[RawWord]) -> (Vec<SttWord>, Vec<AudioEvent>) {
    let mut words = Vec::new();
    let mut events = Vec::new();
    for entry in raw {
        match entry.kind.as_deref() {
            Some("word") => {
                if let (Some(start), Some(end)) = (entry.start, entry.end) {
                    words.push(SttWord {
                        text: entry.text.clone(),
                        start,
                        end,
                        speaker_id: clean_speaker_id(entry.speaker_id.as_deref()),
                    });
                }
            }
            Some("audio_event") => events.push(AudioEvent {
                label: entry.text.clone(),
                start_ms: secs_to_ms(entry.start),
                end_ms: secs_to_ms(entry.end),
            }),
            _ => {}
        }
    }
    (words, events)
}

fn clean_speaker_id(speaker_id: Option<&str>) -> Option<String> {
    speaker_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Convert a seconds offset to whole milliseconds.
///
/// `None`, non-finite, or negative input yields `None`. Otherwise the value is
/// range-checked against the `u32` domain before the cast: out-of-range yields
/// `None` rather than a wrapped or saturated value.
fn secs_to_ms(secs: Option<f32>) -> Option<u32> {
    let secs = secs?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    let ms = (secs * 1000.0).round();
    if ms > f32::from(u16::MAX) * f32::from(u16::MAX) {
        // f32 cannot represent every u32; guard well within the exact-integer
        // range so the truncating cast below stays lossless.
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ms is finite, non-negative, and range-checked above"
    )]
    Some(ms as u32)
}

/// Wrap word-level timing into a single segment spanning the transcript.
///
/// `ElevenLabs` returns flat `words[]` rather than segments, so the whole
/// transcript is one segment bounded by the first and last word with timing.
/// `confidence` stays `None`: the wire `logprob` is not a `[0, 1]` confidence.
/// Empty input (no timed words) yields no segment rather than a zero-span one.
pub fn build_segments(text: &str, words: Vec<SttWord>) -> Vec<SttSegment> {
    let (Some(first), Some(last)) = (words.first(), words.last()) else {
        return Vec::new();
    };
    let speaker_id = uniform_speaker_id(&words);
    vec![SttSegment {
        start: first.start,
        end: last.end,
        text: text.to_owned(),
        speaker_id,
        confidence: None,
        words,
    }]
}

/// Single speaker id when every timed word shares it, else `None`.
pub fn uniform_speaker_id(words: &[SttWord]) -> Option<String> {
    let first = words.first()?.speaker_id.as_deref()?;
    if words
        .iter()
        .all(|word| word.speaker_id.as_deref() == Some(first))
    {
        Some(first.to_owned())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: Option<f32>, end: Option<f32>) -> RawWord {
        RawWord {
            text: text.to_owned(),
            start,
            end,
            kind: Some("word".to_owned()),
            speaker_id: None,
        }
    }

    fn audio_event(text: &str, start: Option<f32>, end: Option<f32>) -> RawWord {
        RawWord {
            text: text.to_owned(),
            start,
            end,
            kind: Some("audio_event".to_owned()),
            speaker_id: None,
        }
    }

    #[test]
    fn words_with_timing_become_stt_words() {
        let raw = vec![
            word("Hey", Some(0.0), Some(0.4)),
            word("world", Some(0.9), Some(1.3)),
        ];
        let (words, events) = parse_words(&raw);
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "Hey");
        assert!((words[1].start - 0.9).abs() < f32::EPSILON);
        assert!(events.is_empty());
    }

    #[test]
    fn word_missing_timing_is_skipped() {
        let raw = vec![word("Hey", None, None), word("world", Some(0.9), Some(1.3))];
        let (words, _) = parse_words(&raw);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "world");
    }

    #[test]
    fn audio_event_maps_to_event_with_millis() {
        let raw = vec![audio_event("(laughter)", Some(0.4), Some(0.9))];
        let (words, events) = parse_words(&raw);
        assert!(words.is_empty());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].label, "(laughter)");
        assert_eq!(events[0].start_ms, Some(400));
        assert_eq!(events[0].end_ms, Some(900));
    }

    #[test]
    fn audio_event_null_timestamps_stay_none() {
        let raw = vec![audio_event("(applause)", None, None)];
        let (_, events) = parse_words(&raw);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].start_ms, None);
        assert_eq!(events[0].end_ms, None);
    }

    #[test]
    fn spacing_and_unknown_kinds_are_skipped() {
        let spacing = RawWord {
            text: " ".to_owned(),
            start: None,
            end: None,
            kind: Some("spacing".to_owned()),
            speaker_id: None,
        };
        let untyped = RawWord {
            text: "x".to_owned(),
            start: Some(0.0),
            end: Some(0.1),
            kind: None,
            speaker_id: None,
        };
        let (words, events) = parse_words(&[spacing, untyped]);
        assert!(words.is_empty());
        assert!(events.is_empty());
    }

    #[test]
    fn negative_and_non_finite_secs_yield_none() {
        assert_eq!(secs_to_ms(None), None);
        assert_eq!(secs_to_ms(Some(-1.0)), None);
        assert_eq!(secs_to_ms(Some(f32::INFINITY)), None);
        assert_eq!(secs_to_ms(Some(f32::NAN)), None);
        assert_eq!(secs_to_ms(Some(1.25)), Some(1250));
    }

    #[test]
    fn preserves_speaker_id_and_ignores_logprob() {
        let raw: RawWord = serde_json::from_value(serde_json::json!({
            "text": "Hi",
            "start": 0.0,
            "end": 0.2,
            "type": "word",
            "speaker_id": "spk_0",
            "logprob": -0.12
        }))
        .expect("valid raw word");
        assert_eq!(raw.text, "Hi");
        let (words, _) = parse_words(&[raw]);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].speaker_id.as_deref(), Some("spk_0"));
    }
}
