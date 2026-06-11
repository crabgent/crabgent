//! `ElevenLabs` audio provider adapter for crabgent.

/// Alias keeps `#[crabgent_log::instrument]` proc-macro expansion (which emits
/// `::tracing::*` paths) resolving to `crabgent_log` without re-introducing a
/// direct `tracing` dep.
extern crate crabgent_log as tracing;

mod batch;
mod config;
mod error;
mod events;
mod forced_alignment;
mod models;
mod provider;
pub mod tts;
mod words;
mod ws;

pub use config::ElevenLabsConfig;
pub use error::ElevenLabsError;
pub use models::ElevenLabsModelId;
pub use provider::ElevenLabsSttProvider;
pub use tts::{ElevenLabsTtsProvider, ElevenLabsVoiceSettings};
pub use ws::SttWsClient;
