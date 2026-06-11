//! Pure functions that derive [`VoiceSignals`] from an [`SttResponse`].
//!
//! Every function here is total and panic-free over arbitrary `f32`
//! timing input, including `NaN`, infinities, and negative or
//! out-of-order timestamps. Non-finite spans collapse to `None`;
//! negative gaps saturate to zero. None of these helpers allocate beyond
//! the references they collect.

use crabgent_core::{SttResponse, SttWord, VoiceSignals};

use crate::config::ProsodyConfig;

/// Flatten every segment's word list into one borrowed slice-like vector,
/// preserving delivery order.
///
/// Invariant: words are assumed chronological (the provider returns them
/// ordered, `first.start <= last.end`). `max_pause_ms` and
/// `speech_rate_wpm` rely on that ordering; out-of-order timing yields a
/// degraded (but still panic-free) result, not a correct one.
fn collect_words(stt: &SttResponse) -> Vec<&SttWord> {
    stt.segments
        .iter()
        .flat_map(|segment| segment.words.iter())
        .collect()
}

/// Deduplicate provider speaker labels in delivery order.
fn speaker_ids(stt: &SttResponse) -> Vec<String> {
    let mut speakers = Vec::new();
    for segment in &stt.segments {
        push_speaker(&mut speakers, segment.speaker_id.as_deref());
        for word in &segment.words {
            push_speaker(&mut speakers, word.speaker_id.as_deref());
        }
    }
    speakers
}

fn push_speaker(speakers: &mut Vec<String>, speaker: Option<&str>) {
    let Some(speaker) = speaker.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    if !speakers.iter().any(|existing| existing == speaker) {
        speakers.push(speaker.to_owned());
    }
}

/// Convert a seconds offset into whole milliseconds.
///
/// Returns `None` for non-finite or negative input. Finite, non-negative
/// values are rounded and clamped to the `u32` range. The `as` cast runs
/// only after the value has been range-checked against `u32::MAX`, so no
/// truncation or sign loss can occur.
fn secs_to_ms_u32(secs: f32) -> Option<u32> {
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    let ms = (secs * 1000.0).round();
    // `u32::MAX` is not exactly representable as `f32`; the nearest f32 is
    // `4294967296.0` (2^32). Comparing against `2^32` keeps the cast in
    // range: any `ms` below it rounds to a value `<= u32::MAX`.
    if ms >= 4_294_967_300.0 {
        return Some(u32::MAX);
    }
    // Range-checked above: 0.0 <= ms < 2^32, so the cast neither wraps nor
    // loses the sign.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ms is finite, non-negative, and < 2^32 after the guards above"
    )]
    Some(ms as u32)
}

/// Inter-word gap in milliseconds for one consecutive `(prev, next)`
/// pair: `next.start - prev.end`, clamped at zero, `None` when either
/// endpoint is non-finite.
fn gap_ms(prev: &SttWord, next: &SttWord) -> Option<u32> {
    if !prev.end.is_finite() || !next.start.is_finite() {
        return None;
    }
    let gap = next.start - prev.end;
    if gap <= 0.0 {
        return Some(0);
    }
    secs_to_ms_u32(gap)
}

/// Iterate the consecutive-pair gaps of a word list as milliseconds,
/// skipping pairs whose timing is non-finite.
fn gaps_ms<'a>(words: &'a [&'a SttWord]) -> impl Iterator<Item = u32> + 'a {
    words.windows(2).filter_map(|pair| match pair {
        [prev, next] => gap_ms(prev, next),
        _ => None,
    })
}

/// Longest silent gap between consecutive words, in milliseconds.
///
/// `None` when there are fewer than two words or every gap is non-finite.
fn max_pause_ms(words: &[&SttWord]) -> Option<u32> {
    gaps_ms(words).max()
}

/// Speaking rate in whole words per minute.
///
/// `None` when there are fewer than two words, the utterance span is
/// non-positive, or either endpoint is non-finite. Otherwise the rate is
/// rounded and clamped to the `u16` range.
fn speech_rate_wpm(words: &[&SttWord]) -> Option<u16> {
    let first = words.first()?;
    let last = words.last()?;
    if words.len() < 2 {
        return None;
    }
    if !first.start.is_finite() || !last.end.is_finite() {
        return None;
    }
    let span = last.end - first.start;
    if !span.is_finite() || span <= 0.0 {
        return None;
    }
    // Compute in f64: f32's 23-bit mantissa would lose precision on the
    // word count, and `usize as f32` trips `cast_precision_loss`. A
    // checked `u32` conversion plus `f64::from` keeps the math exact for
    // any realistic utterance and total for pathological ones.
    let count = u32::try_from(words.len()).unwrap_or(u32::MAX);
    let words_per_min = (f64::from(count) / f64::from(span)) * 60.0;
    if !words_per_min.is_finite() || words_per_min < 0.0 {
        return None;
    }
    let rounded = words_per_min.round();
    if rounded >= f64::from(u16::MAX) {
        return Some(u16::MAX);
    }
    // Range-checked: 0.0 <= rounded < u16::MAX.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded is finite, non-negative, and < u16::MAX after the guards above"
    )]
    Some(rounded as u16)
}

/// Number of consecutive-word gaps strictly above `threshold_ms`.
fn hesitation_count(words: &[&SttWord], threshold_ms: u32) -> u32 {
    gaps_ms(words)
        .filter(|gap| *gap > threshold_ms)
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

/// Compute [`VoiceSignals`] from a transcription response.
///
/// Audio events tagged by the provider are always forwarded: they are the
/// robust primary signal and do not depend on per-word timing. When
/// [`ProsodyConfig::word_timing`] is enabled, pause length, speaking rate,
/// and hesitation count are mined from the flattened word list; otherwise
/// they stay empty.
///
/// Returns `None` only when nothing was derived at all: no audio events,
/// no speakers, no pause, no rate, and a zero hesitation count. Energy
/// band is always `None` here; audio-energy extraction can populate it
/// when available.
pub fn voice_signals(stt: &SttResponse, cfg: &ProsodyConfig) -> Option<VoiceSignals> {
    let audio_events = stt.audio_events.clone();
    let speakers = speaker_ids(stt);

    let (pause_ms, speech_rate, hesitations) = if cfg.word_timing {
        let words = collect_words(stt);
        (
            max_pause_ms(&words),
            speech_rate_wpm(&words),
            hesitation_count(&words, cfg.hesitation_threshold_ms),
        )
    } else {
        (None, None, 0)
    };

    if audio_events.is_empty()
        && speakers.is_empty()
        && pause_ms.is_none()
        && speech_rate.is_none()
        && hesitations == 0
    {
        return None;
    }

    Some(VoiceSignals {
        pause_ms,
        speech_rate_wpm: speech_rate,
        hesitation_count: hesitations,
        audio_events,
        speakers,
        speaker_identities: Vec::new(),
        energy_band: None,
    })
}

#[cfg(test)]
mod tests;
