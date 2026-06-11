//! Configuration for the prosody pipeline.

/// Tunables for deriving [`VoiceSignals`] from an STT response.
///
/// [`VoiceSignals`]: crabgent_core::VoiceSignals
#[derive(Debug, Clone)]
pub struct ProsodyConfig {
    /// When `true`, word-level timing is mined for pause length, speaking
    /// rate, and hesitation count. When `false`, those signals are left
    /// empty and only provider-tagged audio events are forwarded. Disable
    /// it when a provider returns unreliable per-word timestamps.
    pub word_timing: bool,
    /// A silent inter-word gap strictly above this many milliseconds
    /// counts as one hesitation marker. Lower it to flag shorter pauses.
    pub hesitation_threshold_ms: u32,
}

impl Default for ProsodyConfig {
    fn default() -> Self {
        Self {
            word_timing: true,
            hesitation_threshold_ms: 600,
        }
    }
}

/// Thresholds for the cheap text-vs-prosody divergence read.
///
/// Defaults gate conservatively: a clearly slow rate or a clearly long pause
/// reads as flat, a clearly fast rate reads as animated, and the band between
/// is neutral. Tighten the band to flag more, widen it to flag less.
#[derive(Debug, Clone)]
pub struct DivergenceConfig {
    /// A speaking rate at or below this (words per minute) reads as flat,
    /// low-energy delivery.
    pub flat_max_wpm: u16,
    /// A speaking rate at or above this (words per minute) reads as animated,
    /// high-energy delivery.
    pub animated_min_wpm: u16,
    /// A maximum inter-word pause at or above this (milliseconds) reads as
    /// flat delivery.
    pub flat_min_pause_ms: u32,
}

impl Default for DivergenceConfig {
    fn default() -> Self {
        Self {
            flat_max_wpm: 110,
            animated_min_wpm: 200,
            flat_min_pause_ms: 700,
        }
    }
}
