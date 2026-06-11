//! `OpenAI` speech-to-text model catalog.

use crabgent_core::{SttModelId, SttModelInfo};

pub const GPT_4O_TRANSCRIBE: &str = "gpt-4o-transcribe";
pub const GPT_4O_MINI_TRANSCRIBE: &str = "gpt-4o-mini-transcribe";
pub const GPT_REALTIME_WHISPER: &str = "gpt-realtime-whisper";

#[must_use]
pub fn openai_stt_models() -> Vec<SttModelInfo> {
    vec![
        model(GPT_4O_TRANSCRIBE, true),
        model(GPT_4O_MINI_TRANSCRIBE, false),
        model(GPT_REALTIME_WHISPER, true),
    ]
}

fn model(id: &'static str, supports_streaming: bool) -> SttModelInfo {
    SttModelInfo {
        id: SttModelId::new(id),
        supports_streaming,
        supports_diarization: false,
    }
}
