//! Prosody domain: derive paralinguistic [`VoiceSignals`] from an
//! [`SttResponse`] and surface them to the LLM.
//!
//! Three responsibilities, split across modules:
//!
//! - [`signals`]: pure, panic-free functions that compute a
//!   [`VoiceSignals`] from word-level STT timing plus provider-tagged
//!   audio events. No I/O, no allocation beyond the result.
//! - [`divergence`]: [`DivergenceDetector`], a pure, panic-free read of
//!   whether the transcript words contradict the prosodic delivery
//!   (positive words with flat delivery, or negative words with animated
//!   delivery). No paid call, no raw audio decoding.
//! - [`hook`]: a [`crabgent_core::Hook`] implementation that, in
//!   `before_llm`, reads the `voice` object already attached to user
//!   `transcript` content blocks and prepends a single self-closing
//!   `<voice .../>` tag to the block text. The tag is a trust fence: it
//!   lets the model read the prosody summary without parsing nested JSON,
//!   and every attribute value is attribute-escaped against tag-breakout.
//!
//! [`VoiceSignals`]: crabgent_core::VoiceSignals
//! [`SttResponse`]: crabgent_core::SttResponse

mod config;
mod divergence;
mod hook;
mod render;
mod signals;

pub use config::{DivergenceConfig, ProsodyConfig};
pub use divergence::{
    DivergenceConfidence, DivergenceDetector, DivergenceVerdict, ProsodyEnergy, TextPolarity,
};
pub use hook::ProsodyHook;
pub use signals::voice_signals;
