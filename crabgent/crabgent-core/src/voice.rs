//! Voice-perception (paralinguistic) types.
//!
//! `AudioRef` is an opaque handle to audio bytes held by an out-of-core
//! `AudioStore`. `VoiceSignals` carries derived prosodic perception data
//! attached to a [`crate::message::ContentBlock::Transcript`]. These types
//! live in `crabgent-core` because `ContentBlock` references them and the
//! core crate must not depend on channel or store crates.
//!
//! `VoiceSignals` and its members avoid `f32`/`f64` on purpose: they are
//! embedded in `ContentBlock`, which derives `Eq`. Floating point would
//! break that derive. Rates are whole words per minute, pauses are whole
//! milliseconds, loudness is a coarse band.

use serde::{Deserialize, Serialize};

/// Opaque reference to audio bytes held by an `AudioStore`.
///
/// The inner string is a UUID v7 identifier assigned by the store
/// implementation. Callers receive an `AudioRef` from the store's `put`;
/// they must not fabricate one. Mirrors the channel crate's `ImageRef`
/// but lives here because [`crate::message::ContentBlock::Transcript`]
/// embeds it and core cannot depend on channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AudioRef(String);

impl AudioRef {
    /// Wrap a store-generated identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the inner identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Coarse loudness band derived from the audio envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EnergyBand {
    /// Quiet, low-energy delivery.
    Low,
    /// Conversational, mid-energy delivery.
    Medium,
    /// Loud, high-energy delivery.
    High,
}

/// A non-lexical audio event detected during transcription
/// (laughter, applause, sigh, ...).
///
/// `label` is the provider-reported category. Providers differ, so it is
/// free-form rather than a closed enum. `start_ms`/`end_ms` are offsets
/// from the start of the utterance when the provider timestamps the event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioEvent {
    /// Provider category label (for example `"laughter"`).
    pub label: String,
    /// Event start offset in milliseconds, when timestamped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<u32>,
    /// Event end offset in milliseconds, when timestamped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u32>,
}

impl AudioEvent {
    /// Event with a category label and no timestamps.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            start_ms: None,
            end_ms: None,
        }
    }
}

/// A downstream or provider-supplied best guess for a real speaker identity.
///
/// The identity is deliberately generic. Upstream only defines the carrier:
/// deployment-specific profile ids, display names, and matching semantics live
/// outside `crabgent-core`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerIdentity {
    /// Stable deployment-local profile id, for example `"speaker_a"`.
    pub id: String,
    /// Optional human-facing label for the profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// Confidence in the match, clamped by producers to `0..=100`.
    pub confidence: u8,
    /// Source of the identity signal, for example `"claimed"` or
    /// `"voiceprint"`.
    pub source: String,
    /// Provider diarization label this identity maps to, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_label: Option<String>,
}

impl SpeakerIdentity {
    /// Construct an identity guess and clamp confidence to `0..=100`.
    pub fn new(id: impl Into<String>, source: impl Into<String>, confidence: u8) -> Self {
        Self {
            id: id.into(),
            display: None,
            confidence: confidence.min(100),
            source: source.into(),
            speaker_label: None,
        }
    }

    /// Add a human-facing label.
    #[must_use]
    pub fn with_display(mut self, display: impl Into<String>) -> Self {
        self.display = Some(display.into());
        self
    }

    /// Attach the provider diarization label this identity maps to.
    #[must_use]
    pub fn with_speaker_label(mut self, speaker_label: impl Into<String>) -> Self {
        self.speaker_label = Some(speaker_label.into());
        self
    }
}

/// Derived paralinguistic signals attached to a transcript.
///
/// Populated by the prosody pipeline when provider metadata is available.
/// All members are integer or enum typed so `ContentBlock` keeps its `Eq`
/// derive.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VoiceSignals {
    /// Longest silent gap between words, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_ms: Option<u32>,
    /// Speaking rate in whole words per minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_rate_wpm: Option<u16>,
    /// Count of detected hesitation markers (filled pauses, restarts).
    pub hesitation_count: u32,
    /// Non-lexical audio events in delivery order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_events: Vec<AudioEvent>,
    /// Provider speaker labels seen in delivery order, deduplicated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub speakers: Vec<String>,
    /// Deployment-local speaker identity guesses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub speaker_identities: Vec<SpeakerIdentity>,
    /// Coarse loudness band.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_band: Option<EnergyBand>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_ref_serde_transparent() {
        let original = AudioRef::new("01946a3c-7c2b-7d2e-8f1a-5b3c2d1e0f0a");
        let json = serde_json::to_string(&original).expect("serialize");
        assert_eq!(json, "\"01946a3c-7c2b-7d2e-8f1a-5b3c2d1e0f0a\"");
        let back: AudioRef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, back);
    }

    #[test]
    fn audio_ref_as_str_returns_inner() {
        let r = AudioRef::new("test-id");
        assert_eq!(r.as_str(), "test-id");
    }

    #[test]
    fn voice_signals_default_is_empty() {
        let v = VoiceSignals::default();
        assert_eq!(v.pause_ms, None);
        assert_eq!(v.speech_rate_wpm, None);
        assert_eq!(v.hesitation_count, 0);
        assert!(v.audio_events.is_empty());
        assert!(v.speakers.is_empty());
        assert!(v.speaker_identities.is_empty());
        assert_eq!(v.energy_band, None);
    }

    #[test]
    fn voice_signals_serde_roundtrip() {
        let signals = VoiceSignals {
            pause_ms: Some(420),
            speech_rate_wpm: Some(150),
            hesitation_count: 2,
            audio_events: vec![AudioEvent::new("laughter")],
            speakers: vec!["speaker_0".to_owned()],
            speaker_identities: vec![
                SpeakerIdentity::new("speaker_a", "voiceprint", 92)
                    .with_display("Speaker A")
                    .with_speaker_label("speaker_0"),
            ],
            energy_band: Some(EnergyBand::Low),
        };
        let json = serde_json::to_string(&signals).expect("serialize");
        let back: VoiceSignals = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(signals, back);
    }

    #[test]
    fn empty_voice_signals_skips_optional_fields() {
        let json = serde_json::to_string(&VoiceSignals::default()).expect("serialize");
        assert_eq!(json, "{\"hesitation_count\":0}");
    }

    #[test]
    fn speaker_identity_clamps_confidence() {
        let identity = SpeakerIdentity::new("speaker_a", "voiceprint", 255);
        assert_eq!(identity.confidence, 100);
    }

    #[test]
    fn energy_band_serializes_snake_case() {
        let json = serde_json::to_string(&EnergyBand::Medium).expect("serialize");
        assert_eq!(json, "\"medium\"");
    }
}
