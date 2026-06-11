//! Cheap, local, deterministic text-vs-prosody divergence detection.
//!
//! [`DivergenceDetector::detect`] compares a lexical sentiment read of the
//! transcript words against a prosodic-energy read of the delivery. A
//! divergence is a contradiction: positive words spoken with flat, low-energy
//! delivery (the bitter "ja super"), or negative words spoken with animated,
//! high-energy delivery.
//!
//! Both reads are heuristic and intentionally coarse. The lexical side is a
//! small curated word list, not a model; the prosodic side reuses the already
//! populated [`VoiceSignals`] fields (pause length, speaking rate, audio
//! events), never a paid call and never raw audio decoding. The verdict gates
//! conservatively: only a strong, unambiguous contradiction reads as
//! [`DivergenceConfidence::High`], because lexical sentiment is fuzzy and the
//! consumer-side audio call this gates is expensive.
//!
//! Every function here is total and panic-free over arbitrary input.

use std::cmp::Ordering;

use crabgent_core::{AudioEvent, VoiceSignals};

use crate::config::DivergenceConfig;

/// Lexical sentiment of the transcript words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TextPolarity {
    /// Positive/enthusiastic wording dominates.
    Positive,
    /// Negative/critical wording dominates.
    Negative,
    /// No clear lexical sentiment.
    Neutral,
}

/// Coarse prosodic-energy read derived from populated [`VoiceSignals`] fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProsodyEnergy {
    /// Slow, long-paused, low-energy delivery.
    Flat,
    /// Neither clearly flat nor animated, or no usable timing.
    Neutral,
    /// Fast or laughter-bearing, high-energy delivery.
    Animated,
}

/// Confidence that the text and the prosody genuinely contradict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DivergenceConfidence {
    /// No contradiction (the default for any non-diverging verdict).
    Low,
    /// A contradiction, but a weak or ambiguous one.
    Medium,
    /// A strong, unambiguous contradiction. The only level that routes.
    High,
}

/// Outcome of a cheap, local text-vs-prosody divergence read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DivergenceVerdict {
    /// Whether the text and the prosody contradict at all.
    pub diverges: bool,
    /// How confident the read is. `High` is the only routing level.
    pub confidence: DivergenceConfidence,
    /// The cheap prosody summary that fed the read.
    pub prosody: ProsodyEnergy,
    /// The lexical sentiment that fed the read.
    pub text_polarity: TextPolarity,
}

/// Positive/enthusiastic markers (German + English), lowercase.
const POSITIVE_WORDS: &[&str] = &[
    "super",
    "toll",
    "geil",
    "klasse",
    "prima",
    "perfekt",
    "spitze",
    "wunderbar",
    "freue",
    "danke",
    "great",
    "awesome",
    "perfect",
    "love",
    "amazing",
    "wonderful",
    "fantastic",
    "nice",
    "thanks",
    "excellent",
];

/// Negative/critical markers (German + English), lowercase.
const NEGATIVE_WORDS: &[&str] = &[
    "schlecht",
    "mist",
    "furchtbar",
    "schrecklich",
    "blöd",
    "nervig",
    "hasse",
    "katastrophe",
    "ätzend",
    "grauenhaft",
    "terrible",
    "awful",
    "hate",
    "worst",
    "horrible",
    "bad",
    "annoying",
    "useless",
    "disgusting",
    "ugh",
];

/// Cheap, local, deterministic text-vs-prosody contradiction detector.
///
/// Stateless apart from its [`DivergenceConfig`] thresholds; cloneable and
/// shareable. Construct with [`DivergenceDetector::new`] or via `Default`.
#[derive(Debug, Clone, Default)]
pub struct DivergenceDetector {
    cfg: DivergenceConfig,
}

impl DivergenceDetector {
    /// Detector with explicit thresholds.
    #[must_use]
    pub const fn new(cfg: DivergenceConfig) -> Self {
        Self { cfg }
    }

    /// Read whether the transcript words and the delivery contradict.
    ///
    /// Total and panic-free. Degenerate input (empty text, no usable voice
    /// signals) reads as neutral on both axes and never diverges.
    #[must_use]
    pub fn detect(&self, text: &str, voice: &VoiceSignals) -> DivergenceVerdict {
        let (text_polarity, text_clear) = classify_text(text);
        let (prosody, prosody_strong) = self.classify_prosody(voice);
        let (diverges, confidence) = assess(text_polarity, text_clear, prosody, prosody_strong);
        DivergenceVerdict {
            diverges,
            confidence,
            prosody,
            text_polarity,
        }
    }

    /// Classify delivery energy from the populated voice fields. Returns the
    /// band plus whether the read is strong (laughter, or both flat cues).
    fn classify_prosody(&self, voice: &VoiceSignals) -> (ProsodyEnergy, bool) {
        let laughter = has_laughter(&voice.audio_events);
        let fast = voice
            .speech_rate_wpm
            .is_some_and(|wpm| wpm >= self.cfg.animated_min_wpm);
        if laughter || fast {
            // Laughter is an unambiguous high-energy cue; a fast rate alone is
            // weaker, so strength tracks laughter only.
            return (ProsodyEnergy::Animated, laughter);
        }

        let slow = voice
            .speech_rate_wpm
            .is_some_and(|wpm| wpm <= self.cfg.flat_max_wpm);
        let long_pause = voice
            .pause_ms
            .is_some_and(|pause| pause >= self.cfg.flat_min_pause_ms);
        if slow || long_pause {
            return (ProsodyEnergy::Flat, slow && long_pause);
        }

        (ProsodyEnergy::Neutral, false)
    }
}

/// Whether any audio event reads as laughter.
fn has_laughter(events: &[AudioEvent]) -> bool {
    events
        .iter()
        .any(|event| event.label.to_lowercase().contains("laugh"))
}

/// Lowercase the text, split on non-alphanumeric boundaries, and tally curated
/// positive/negative markers. Returns the dominant polarity plus whether it is
/// unambiguous (no markers of the opposite sign).
fn classify_text(text: &str) -> (TextPolarity, bool) {
    let (mut pos, mut neg) = (0u32, 0u32);
    for token in tokenize(text) {
        if POSITIVE_WORDS.contains(&token.as_str()) {
            pos += 1;
        } else if NEGATIVE_WORDS.contains(&token.as_str()) {
            neg += 1;
        }
    }
    match pos.cmp(&neg) {
        Ordering::Greater => (TextPolarity::Positive, neg == 0),
        Ordering::Less => (TextPolarity::Negative, pos == 0),
        Ordering::Equal => (TextPolarity::Neutral, false),
    }
}

/// Yield lowercased alphanumeric word tokens, skipping empties. Unicode-aware,
/// so German umlauts and `ß` survive.
fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

/// Decide divergence and confidence from the two reads.
///
/// A divergence is a positive/flat or negative/animated contradiction. It is
/// `High` only when both reads are strong and unambiguous, else `Medium`. No
/// contradiction is always `Low`.
const fn assess(
    text: TextPolarity,
    text_clear: bool,
    prosody: ProsodyEnergy,
    prosody_strong: bool,
) -> (bool, DivergenceConfidence) {
    let diverges = matches!(
        (text, prosody),
        (TextPolarity::Positive, ProsodyEnergy::Flat)
            | (TextPolarity::Negative, ProsodyEnergy::Animated)
    );
    if !diverges {
        return (false, DivergenceConfidence::Low);
    }
    let confidence = if text_clear && prosody_strong {
        DivergenceConfidence::High
    } else {
        DivergenceConfidence::Medium
    };
    (true, confidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crabgent_core::AudioEvent;
    use proptest::prelude::*;

    /// A flat, low-energy delivery: slow rate plus a long pause, no laughter.
    fn flat_voice() -> VoiceSignals {
        VoiceSignals {
            pause_ms: Some(1200),
            speech_rate_wpm: Some(80),
            hesitation_count: 1,
            audio_events: Vec::new(),
            speakers: Vec::new(),
            speaker_identities: Vec::new(),
            energy_band: None,
        }
    }

    /// An animated, high-energy delivery: fast rate plus laughter.
    fn animated_voice() -> VoiceSignals {
        VoiceSignals {
            pause_ms: Some(50),
            speech_rate_wpm: Some(230),
            hesitation_count: 0,
            audio_events: vec![AudioEvent::new("laughter")],
            speakers: Vec::new(),
            speaker_identities: Vec::new(),
            energy_band: None,
        }
    }

    #[test]
    fn flat_positive_is_high_divergence() {
        let verdict = DivergenceDetector::default().detect("ja super", &flat_voice());
        assert!(verdict.diverges);
        assert_eq!(verdict.confidence, DivergenceConfidence::High);
        assert_eq!(verdict.prosody, ProsodyEnergy::Flat);
        assert_eq!(verdict.text_polarity, TextPolarity::Positive);
    }

    #[test]
    fn congruent_positive_animated_does_not_diverge() {
        let verdict = DivergenceDetector::default().detect("ja super!", &animated_voice());
        assert!(!verdict.diverges);
        assert_eq!(verdict.confidence, DivergenceConfidence::Low);
        assert_eq!(verdict.prosody, ProsodyEnergy::Animated);
        assert_eq!(verdict.text_polarity, TextPolarity::Positive);
    }

    #[test]
    fn negative_animated_diverges() {
        let verdict = DivergenceDetector::default().detect("das ist furchtbar", &animated_voice());
        assert!(verdict.diverges);
        assert_eq!(verdict.confidence, DivergenceConfidence::High);
        assert_eq!(verdict.text_polarity, TextPolarity::Negative);
    }

    #[test]
    fn empty_text_and_empty_voice_do_not_diverge() {
        let verdict = DivergenceDetector::default().detect("", &VoiceSignals::default());
        assert!(!verdict.diverges);
        assert_eq!(verdict.confidence, DivergenceConfidence::Low);
        assert_eq!(verdict.prosody, ProsodyEnergy::Neutral);
        assert_eq!(verdict.text_polarity, TextPolarity::Neutral);
    }

    #[test]
    fn neutral_text_with_flat_voice_does_not_diverge() {
        let verdict = DivergenceDetector::default().detect("the meeting is at noon", &flat_voice());
        assert!(!verdict.diverges);
        assert_eq!(verdict.text_polarity, TextPolarity::Neutral);
    }

    #[test]
    fn mixed_polarity_words_are_not_clear_so_only_medium() {
        // "super" (pos) + "mist" (neg) with one extra positive tips Positive
        // but not cleanly: a negative marker is present, so confidence is at
        // most Medium even on a flat delivery.
        let verdict =
            DivergenceDetector::default().detect("super aber mist und toll", &flat_voice());
        assert_eq!(verdict.text_polarity, TextPolarity::Positive);
        assert!(verdict.diverges);
        assert_eq!(verdict.confidence, DivergenceConfidence::Medium);
    }

    #[test]
    fn weak_flat_only_long_pause_is_medium() {
        // Only a long pause, no slow rate: Flat but not strong -> Medium.
        let voice = bare_voice(Some(1500), None);
        let verdict = DivergenceDetector::default().detect("super", &voice);
        assert!(verdict.diverges);
        assert_eq!(verdict.confidence, DivergenceConfidence::Medium);
    }

    #[test]
    fn umlaut_negative_word_matches() {
        let verdict = DivergenceDetector::default().detect("voll ätzend", &animated_voice());
        assert_eq!(verdict.text_polarity, TextPolarity::Negative);
        assert!(verdict.diverges);
    }

    fn bare_voice(pause: Option<u32>, wpm: Option<u16>) -> VoiceSignals {
        VoiceSignals {
            pause_ms: pause,
            speech_rate_wpm: wpm,
            hesitation_count: 0,
            audio_events: Vec::new(),
            speakers: Vec::new(),
            speaker_identities: Vec::new(),
            energy_band: None,
        }
    }

    proptest! {
        #[test]
        fn never_panics_on_arbitrary_input(
            text in ".*",
            pause in proptest::option::of(0u32..600_000),
            wpm in proptest::option::of(0u16..1000),
            hesitations in 0u32..1000,
            labels in proptest::collection::vec("[a-zä ]{0,12}", 0..6),
        ) {
            let voice = VoiceSignals {
                pause_ms: pause,
                speech_rate_wpm: wpm,
                hesitation_count: hesitations,
                audio_events: labels.iter().map(AudioEvent::new).collect(),
                speakers: Vec::new(),
                speaker_identities: Vec::new(),
                energy_band: None,
            };
            // Totality plus internal consistency over the full input surface
            // (audio events and hesitation count included): the call never
            // panics and the verdict stays self-consistent.
            let verdict = DivergenceDetector::default().detect(&text, &voice);
            prop_assert_eq!(verdict.diverges, verdict.confidence != DivergenceConfidence::Low);
        }

        #[test]
        fn diverges_iff_confidence_above_low(
            text in ".*",
            pause in proptest::option::of(0u32..600_000),
            wpm in proptest::option::of(0u16..1000),
        ) {
            let voice = bare_voice(pause, wpm);
            let verdict = DivergenceDetector::default().detect(&text, &voice);
            // The verdict is internally consistent: diverging exactly when the
            // confidence is above Low, and never the reverse.
            prop_assert_eq!(verdict.diverges, verdict.confidence != DivergenceConfidence::Low);
        }
    }
}
