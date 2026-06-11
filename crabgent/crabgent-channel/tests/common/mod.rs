//! Shared stubs and event builders for the `stt_inbox` integration test
//! binaries (`stt_inbox.rs` and `stt_inbox_retain.rs`).
//!
//! `tests/common/mod.rs` is a module, not its own test binary, so both
//! integration files can `mod common;` it without duplicating the stubs.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::{
    ChannelError, ChannelInbox, InboundEvent, MessageRef, Participant, ParticipantRole,
};
use crabgent_core::{
    AudioPayload, ContentBlock, Owner, SttError, SttEventStream, SttModelId, SttModelInfo,
    SttProvider, SttProviderCapabilities, SttRequest, SttResponse,
};

#[derive(Clone)]
pub struct RecordingInbox {
    received: Arc<Mutex<Vec<InboundEvent>>>,
}

#[async_trait]
impl ChannelInbox for RecordingInbox {
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        self.received
            .lock()
            .expect("mutex should not be poisoned")
            .push(event);
        Ok(())
    }
}

pub struct ScriptedSttProvider {
    responses: Mutex<VecDeque<String>>,
    error: Option<SttError>,
    requests: Mutex<Vec<SttRequest>>,
}

impl ScriptedSttProvider {
    pub fn responding(responses: &[&str]) -> Self {
        Self {
            responses: Mutex::new(responses.iter().map(ToString::to_string).collect()),
            error: None,
            requests: Mutex::new(Vec::new()),
        }
    }

    #[allow(
        dead_code,
        reason = "used only by the stt_inbox binary; unused in stt_inbox_retain"
    )]
    pub const fn failing(error: SttError) -> Self {
        Self {
            responses: Mutex::new(VecDeque::new()),
            error: Some(error),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn call_count(&self) -> usize {
        self.requests
            .lock()
            .expect("mutex should not be poisoned")
            .len()
    }
}

#[async_trait]
impl SttProvider for ScriptedSttProvider {
    async fn transcribe(&self, req: SttRequest) -> Result<SttResponse, SttError> {
        self.requests
            .lock()
            .expect("mutex should not be poisoned")
            .push(req.clone());
        if let Some(error) = self.error.clone() {
            return Err(error);
        }

        let text = self
            .responses
            .lock()
            .expect("test result")
            .pop_front()
            .unwrap_or_default();
        Ok(SttResponse {
            text,
            model: req.model,
            segments: Vec::new(),
            audio_events: Vec::new(),
            language: None,
        })
    }

    async fn stream(&self, _req: SttRequest) -> Result<SttEventStream, SttError> {
        Err(SttError::Backend("scripted streaming unsupported".into()))
    }

    fn capabilities(&self) -> SttProviderCapabilities {
        SttProviderCapabilities {
            streaming: false,
            audio: true,
        }
    }

    fn models(&self) -> Vec<SttModelInfo> {
        vec![SttModelInfo {
            id: SttModelId::new("scripted-stt"),
            supports_streaming: false,
            supports_diarization: false,
        }]
    }
}

pub fn recording() -> (RecordingInbox, Arc<Mutex<Vec<InboundEvent>>>) {
    let received = Arc::new(Mutex::new(Vec::new()));
    (
        RecordingInbox {
            received: Arc::clone(&received),
        },
        received,
    )
}

pub fn event_with(attachments: Vec<ContentBlock>) -> InboundEvent {
    InboundEvent {
        channel: "slack".into(),
        conv: Owner::new("slack:T1/D1"),
        kind: None,
        from: Participant::new("U1", ParticipantRole::Human),
        message: MessageRef::top_level("slack", Owner::new("slack:T1/D1"), "ts:1"),
        body: "voice note".into(),
        attachments,
        timestamp: Utc::now(),
    }
}

pub fn audio(filename: &str) -> ContentBlock {
    ContentBlock::Audio(
        AudioPayload::new(vec![b'a'], "audio/ogg", Some(filename.into()))
            .expect("valid audio payload"),
    )
}
