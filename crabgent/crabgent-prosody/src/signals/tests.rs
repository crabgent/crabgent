use super::*;

use crabgent_core::{AudioEvent, SttModelId, SttResponse, SttSegment, SttWord};
use proptest::prelude::*;

fn word(text: &str, start: f32, end: f32) -> SttWord {
    SttWord {
        text: text.to_owned(),
        start,
        end,
        speaker_id: None,
    }
}

fn word_with_speaker(text: &str, start: f32, end: f32, speaker_id: &str) -> SttWord {
    SttWord {
        text: text.to_owned(),
        start,
        end,
        speaker_id: Some(speaker_id.to_owned()),
    }
}

fn response(segments: Vec<SttSegment>, audio_events: Vec<AudioEvent>) -> SttResponse {
    SttResponse {
        text: String::new(),
        model: SttModelId::new("test"),
        segments,
        audio_events,
        language: None,
    }
}

fn one_segment(words: Vec<SttWord>) -> Vec<SttSegment> {
    vec![SttSegment {
        start: 0.0,
        end: 0.0,
        text: String::new(),
        speaker_id: None,
        confidence: None,
        words,
    }]
}

#[test]
fn secs_to_ms_rejects_non_finite_and_negative() {
    assert_eq!(secs_to_ms_u32(f32::NAN), None);
    assert_eq!(secs_to_ms_u32(f32::INFINITY), None);
    assert_eq!(secs_to_ms_u32(f32::NEG_INFINITY), None);
    assert_eq!(secs_to_ms_u32(-1.0), None);
    assert_eq!(secs_to_ms_u32(0.0), Some(0));
    assert_eq!(secs_to_ms_u32(1.5), Some(1500));
}

#[test]
fn secs_to_ms_clamps_huge_values() {
    assert_eq!(secs_to_ms_u32(f32::MAX), Some(u32::MAX));
}

#[test]
fn collect_words_flattens_segments_in_order() {
    let segments = vec![
        SttSegment {
            start: 0.0,
            end: 1.0,
            text: String::new(),
            speaker_id: None,
            confidence: None,
            words: vec![word("a", 0.0, 0.4)],
        },
        SttSegment {
            start: 1.0,
            end: 2.0,
            text: String::new(),
            speaker_id: None,
            confidence: None,
            words: vec![word("b", 1.0, 1.4), word("c", 1.5, 1.9)],
        },
    ];
    let stt = response(segments, Vec::new());
    let words = collect_words(&stt);
    let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    assert_eq!(texts, ["a", "b", "c"]);
}

#[test]
fn max_pause_picks_largest_gap() {
    let words = [
        word("a", 0.0, 0.5),
        word("b", 1.0, 1.5),
        word("c", 3.0, 3.5),
    ];
    let refs: Vec<&SttWord> = words.iter().collect();
    // gaps: 1.0-0.5 = 0.5s = 500ms, 3.0-1.5 = 1.5s = 1500ms.
    assert_eq!(max_pause_ms(&refs), Some(1500));
}

#[test]
fn overlapping_and_negative_gaps_clamp_to_zero() {
    // Second word starts before the first ends (overlap).
    let words = [word("a", 0.0, 1.0), word("b", 0.5, 1.2)];
    let refs: Vec<&SttWord> = words.iter().collect();
    assert_eq!(max_pause_ms(&refs), Some(0));
}

#[test]
fn single_word_has_no_pause_or_rate() {
    let words = [word("solo", 0.0, 0.4)];
    let refs: Vec<&SttWord> = words.iter().collect();
    assert_eq!(max_pause_ms(&refs), None);
    assert_eq!(speech_rate_wpm(&refs), None);
    assert_eq!(hesitation_count(&refs, 100), 0);
}

#[test]
fn empty_words_yield_none_and_zero() {
    let refs: Vec<&SttWord> = Vec::new();
    assert_eq!(max_pause_ms(&refs), None);
    assert_eq!(speech_rate_wpm(&refs), None);
    assert_eq!(hesitation_count(&refs, 100), 0);
}

#[test]
fn nan_and_inf_timings_return_none_no_panic() {
    let words = [word("a", f32::NAN, f32::NAN), word("b", f32::INFINITY, 2.0)];
    let refs: Vec<&SttWord> = words.iter().collect();
    // gap_ms skips non-finite endpoints, so there is no valid gap.
    assert_eq!(max_pause_ms(&refs), None);
    assert_eq!(speech_rate_wpm(&refs), None);
    assert_eq!(hesitation_count(&refs, 0), 0);
}

#[test]
fn speech_rate_two_words_over_one_second() {
    // 2 words across a 1.0s span -> 120 wpm.
    let words = [word("a", 0.0, 0.4), word("b", 0.6, 1.0)];
    let refs: Vec<&SttWord> = words.iter().collect();
    assert_eq!(speech_rate_wpm(&refs), Some(120));
}

#[test]
fn speech_rate_zero_span_is_none() {
    let words = [word("a", 1.0, 1.0), word("b", 1.0, 1.0)];
    let refs: Vec<&SttWord> = words.iter().collect();
    assert_eq!(speech_rate_wpm(&refs), None);
}

#[test]
fn speech_rate_saturates_at_u16_max() {
    // 100 zero-duration words spread across a ~10ms span -> roughly
    // 600000 wpm, far above u16::MAX. The result must saturate to
    // u16::MAX, never truncate or wrap to a small value.
    let words: Vec<SttWord> = (0u16..100)
        .map(|i| {
            let t = f32::from(i) * 0.0001;
            word("w", t, t)
        })
        .collect();
    let refs: Vec<&SttWord> = words.iter().collect();
    assert_eq!(speech_rate_wpm(&refs), Some(u16::MAX));
}

#[test]
fn hesitation_counts_gaps_above_threshold() {
    let words = [
        word("a", 0.0, 0.5),
        word("b", 1.0, 1.5), // gap 500ms
        word("c", 2.3, 2.8), // gap 800ms
        word("d", 3.6, 4.0), // gap 800ms
    ];
    let refs: Vec<&SttWord> = words.iter().collect();
    // threshold 600 -> two gaps (800, 800) above it.
    assert_eq!(hesitation_count(&refs, 600), 2);
    // threshold exactly at a gap value -> strict >, so 800 still counts
    // but 500 does not; 800 == 800 boundary excluded.
    assert_eq!(hesitation_count(&refs, 800), 0);
}

#[test]
fn voice_signals_forwards_audio_events_with_empty_words() {
    let stt = response(Vec::new(), vec![AudioEvent::new("laughter")]);
    let signals = voice_signals(&stt, &ProsodyConfig::default()).expect("some signals");
    assert_eq!(signals.audio_events.len(), 1);
    assert_eq!(signals.pause_ms, None);
    assert_eq!(signals.speech_rate_wpm, None);
    assert_eq!(signals.hesitation_count, 0);
    assert!(signals.speakers.is_empty());
    assert_eq!(signals.energy_band, None);
}

#[test]
fn voice_signals_forwards_speakers_with_timing_disabled() {
    let segments = vec![SttSegment {
        start: 0.0,
        end: 2.0,
        text: "a b".to_owned(),
        speaker_id: Some("speaker_0".to_owned()),
        confidence: None,
        words: vec![
            word_with_speaker("a", 0.0, 0.5, "speaker_0"),
            word_with_speaker("b", 1.0, 1.5, "speaker_1"),
            word_with_speaker("c", 1.6, 1.8, "speaker_0"),
        ],
    }];
    let stt = response(segments, Vec::new());
    let cfg = ProsodyConfig {
        word_timing: false,
        ..ProsodyConfig::default()
    };
    let signals = voice_signals(&stt, &cfg).expect("speaker signals");

    assert_eq!(signals.speakers, ["speaker_0", "speaker_1"]);
    assert_eq!(signals.pause_ms, None);
    assert_eq!(signals.speech_rate_wpm, None);
}

#[test]
fn voice_signals_none_when_nothing_present() {
    let stt = response(Vec::new(), Vec::new());
    assert_eq!(voice_signals(&stt, &ProsodyConfig::default()), None);
}

#[test]
fn voice_signals_word_timing_disabled_skips_timing() {
    let segments = one_segment(vec![word("a", 0.0, 0.5), word("b", 2.0, 2.5)]);
    let stt = response(segments, Vec::new());
    let cfg = ProsodyConfig {
        word_timing: false,
        ..ProsodyConfig::default()
    };
    // Word timing off and no audio events -> nothing derived.
    assert_eq!(voice_signals(&stt, &cfg), None);
}

#[test]
fn voice_signals_mines_word_timing_when_enabled() {
    let segments = one_segment(vec![
        word("a", 0.0, 0.5),
        word("b", 2.0, 2.5), // gap 1500ms
    ]);
    let stt = response(segments, Vec::new());
    let signals = voice_signals(&stt, &ProsodyConfig::default()).expect("some signals");
    assert_eq!(signals.pause_ms, Some(1500));
    assert!(signals.speech_rate_wpm.is_some());
    assert_eq!(signals.hesitation_count, 1);
}

fn finite_word_strategy() -> impl Strategy<Value = (f32, f32)> {
    (0.0f32..10_000.0, 0.0f32..10_000.0)
}

proptest! {
    #[test]
    fn never_panics_on_bounded_finite_words(
        timings in proptest::collection::vec(finite_word_strategy(), 0..32),
    ) {
        let words: Vec<SttWord> = timings
            .iter()
            .map(|(start, end)| word("w", *start, *end))
            .collect();
        let refs: Vec<&SttWord> = words.iter().collect();
        // No assertion beyond "does not panic"; the functions are total.
        let _ = max_pause_ms(&refs);
        let _ = speech_rate_wpm(&refs);
        let _ = hesitation_count(&refs, 500);
    }

    #[test]
    fn speech_rate_is_total_over_finite_words(
        timings in proptest::collection::vec(finite_word_strategy(), 2..32),
    ) {
        let words: Vec<SttWord> = timings
            .iter()
            .map(|(start, end)| word("w", *start, *end))
            .collect();
        let refs: Vec<&SttWord> = words.iter().collect();
        // The function is total: it returns either a clamped `u16`
        // (intrinsically <= u16::MAX) or `None`. Asserting the result
        // does not panic is the property; a `<= u16::MAX` check would
        // be a tautology on the return type.
        let _ = speech_rate_wpm(&refs);
    }

    #[test]
    fn empty_words_forwards_audio_events_only(
        labels in proptest::collection::vec("[a-z]{1,8}", 0..8),
    ) {
        let events: Vec<AudioEvent> = labels.iter().map(AudioEvent::new).collect();
        let expect_some = !events.is_empty();
        let stt = response(Vec::new(), events.clone());
        let signals = voice_signals(&stt, &ProsodyConfig::default());
        if expect_some {
            let signals = signals.expect("audio events present");
            prop_assert_eq!(signals.audio_events.len(), events.len());
            prop_assert_eq!(signals.pause_ms, None);
            prop_assert_eq!(signals.speech_rate_wpm, None);
            prop_assert_eq!(signals.hesitation_count, 0);
        } else {
            prop_assert!(signals.is_none());
        }
    }

    #[test]
    fn larger_inserted_gap_does_not_decrease_max_pause(
        base in proptest::collection::vec(finite_word_strategy(), 2..16),
    ) {
        let words: Vec<SttWord> = base
            .iter()
            .map(|(start, end)| word("w", start.min(*end), start.max(*end)))
            .collect();
        let refs: Vec<&SttWord> = words.iter().collect();
        let before = max_pause_ms(&refs).unwrap_or(0);

        // Append a word that opens a strictly larger gap than any
        // existing pause: start it far past the previous word's end.
        let last_end = words.last().map_or(0.0, |w| w.end);
        let mut grown = words.clone();
        let huge_start = last_end + f32::from(u16::MAX);
        grown.push(word("x", huge_start, huge_start + 0.1));
        let grown_refs: Vec<&SttWord> = grown.iter().collect();
        let after = max_pause_ms(&grown_refs).unwrap_or(0);

        prop_assert!(after >= before);
    }
}
