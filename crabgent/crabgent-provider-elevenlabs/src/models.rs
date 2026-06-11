//! `ElevenLabs` STT model catalog.

use crabgent_core::{SttModelId, SttModelInfo};

pub const SCRIBE_V2: &str = "scribe_v2";
pub const SCRIBE_V1: &str = "scribe_v1";
pub const SCRIBE_V2_REALTIME: &str = "scribe_v2_realtime";

/// Stable identifier wrapper for `ElevenLabs` STT models.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ElevenLabsModelId(SttModelId);

impl ElevenLabsModelId {
    pub fn new(id: impl Into<SttModelId>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub const fn as_stt_model_id(&self) -> &SttModelId {
        &self.0
    }
}

impl From<ElevenLabsModelId> for SttModelId {
    fn from(value: ElevenLabsModelId) -> Self {
        value.0
    }
}

#[must_use]
pub fn elevenlabs_stt_models() -> Vec<SttModelInfo> {
    vec![
        model(SCRIBE_V2, false, true),
        model(SCRIBE_V1, false, false),
        model(SCRIBE_V2_REALTIME, true, true),
    ]
}

fn model(id: &'static str, supports_streaming: bool, supports_diarization: bool) -> SttModelInfo {
    SttModelInfo {
        id: SttModelId::new(id),
        supports_streaming,
        supports_diarization,
    }
}
