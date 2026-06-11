//! Integration tests for `SttInbox` prosody signal attachment (Prosody design B4).
//!
//! Each test wires a stub `SttProvider` that returns a controlled
//! `SttResponse`, then asserts that `ContentBlock::Transcript.voice` is
//! populated (or absent) as the prosody computation expects.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use crabgent_channel::{
    AudioStore, AudioStoreError, ChannelError, ChannelInbox, InboundEvent, MessageRef, Participant,
    ParticipantRole, SttInbox,
};
use crabgent_core::{
    AllowAllPolicy, AudioPayload, AudioRef, ContentBlock, Owner, SttError, SttEventStream,
    SttModelId, SttModelInfo, SttProvider, SttProviderCapabilities, SttRequest, SttResponse,
    SttSegment, SttWord,
};

// ---------------------------------------------------------------------------
// Stubs
// ---------------------------------------------------------------------------

/// Stub `AudioStore` that always succeeds with a deterministic `AudioRef`.
struct SucceedingStore;

#[async_trait]
impl AudioStore for SucceedingStore {
    async fn put(&self, _bytes: Bytes, _mime: &str) -> Result<AudioRef, AudioStoreError> {
        Ok(AudioRef::new("stub-ref"))
    }

    async fn get(&self, _audio_ref: &AudioRef) -> Result<(Bytes, String), AudioStoreError> {
        Err(AudioStoreError::NotFound)
    }
}

/// Stub `SttProvider` returning a single pre-built `SttResponse`.
struct FixedSttProvider {
    response: Mutex<Option<SttResponse>>,
}

impl FixedSttProvider {
    const fn once(response: SttResponse) -> Self {
        Self {
            response: Mutex::new(Some(response)),
        }
    }
}

#[async_trait]
impl SttProvider for FixedSttProvider {
    async fn transcribe(&self, req: SttRequest) -> Result<SttResponse, SttError> {
        let mut guard = self.response.lock().expect("mutex should not be poisoned");
        // Return the scripted response once, then fall back to an empty one.
        Ok(guard.take().unwrap_or_else(|| SttResponse {
            text: String::new(),
            model: req.model,
            segments: Vec::new(),
            audio_events: Vec::new(),
            language: None,
        }))
    }

    async fn stream(&self, _req: SttRequest) -> Result<SttEventStream, SttError> {
        Err(SttError::Backend("stub streaming unsupported".into()))
    }

    fn capabilities(&self) -> SttProviderCapabilities {
        SttProviderCapabilities {
            streaming: false,
            audio: true,
        }
    }

    fn models(&self) -> Vec<SttModelInfo> {
        vec![SttModelInfo {
            id: SttModelId::new("stub-stt"),
            supports_streaming: false,
            supports_diarization: false,
        }]
    }
}

/// Stub `ChannelInbox` that collects received events.
#[derive(Clone)]
struct RecordingInbox {
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn recording() -> (RecordingInbox, Arc<Mutex<Vec<InboundEvent>>>) {
    let received = Arc::new(Mutex::new(Vec::new()));
    (
        RecordingInbox {
            received: Arc::clone(&received),
        },
        received,
    )
}

fn event_with(attachment: ContentBlock) -> InboundEvent {
    InboundEvent {
        channel: "slack".into(),
        conv: Owner::new("slack:T1/D1"),
        kind: None,
        from: Participant::new("U1", ParticipantRole::Human),
        message: MessageRef::top_level("slack", Owner::new("slack:T1/D1"), "ts:1"),
        body: "voice note".into(),
        attachments: vec![attachment],
        timestamp: Utc::now(),
    }
}

fn audio_block() -> ContentBlock {
    ContentBlock::Audio(
        AudioPayload::new(vec![b'a'], "audio/ogg", Some("note.ogg".into()))
            .expect("valid audio payload"),
    )
}

fn word(text: &str, start: f32, end: f32) -> SttWord {
    SttWord {
        text: text.to_owned(),
        start,
        end,
        speaker_id: None,
    }
}

const fn segment_with_words(words: Vec<SttWord>) -> SttSegment {
    SttSegment {
        start: 0.0,
        end: 0.0,
        text: String::new(),
        speaker_id: None,
        confidence: None,
        words,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Two words with a 1.5 s gap should produce a Transcript whose `voice`
/// carries `pause_ms` and `speech_rate_wpm`.
#[tokio::test]
async fn voice_attached_when_words_present() {
    let response = SttResponse {
        text: "hello world".into(),
        model: SttModelId::new("stub-stt"),
        segments: vec![segment_with_words(vec![
            word("hello", 0.0, 0.5),
            word("world", 2.0, 2.5), // 1.5 s gap = 1500 ms
        ])],
        audio_events: Vec::new(),
        language: None,
    };

    let provider = Arc::new(FixedSttProvider::once(response));
    let (next, received) = recording();
    let inbox = SttInbox::new(provider, next)
        .with_audio_store(Arc::new(SucceedingStore))
        .with_policy(Arc::new(AllowAllPolicy));

    inbox
        .receive(event_with(audio_block()))
        .await
        .expect("receive succeeded");

    let guard = received.lock().expect("mutex should not be poisoned");
    let ContentBlock::Transcript { voice, .. } = &guard[0].attachments[0] else {
        panic!(
            "expected Transcript block, got {:?}",
            &guard[0].attachments[0]
        );
    };
    let signals = voice
        .as_ref()
        .expect("VoiceSignals should be Some when timing words are present");
    assert_eq!(
        signals.pause_ms,
        Some(1500),
        "pause_ms should reflect the 1500 ms inter-word gap"
    );
    assert!(
        signals.speech_rate_wpm.is_some(),
        "speech_rate_wpm should be Some for two words with a positive span"
    );
}

/// The no-store collapse path must NOT attach voice signals, even for a
/// response whose timing words would yield voice on the retain path. This
/// pins the intentional asymmetry: `retain_one` enriches a `Transcript`
/// block with `VoiceSignals`, while `collapse_transcribe` folds plain
/// `Text` (which has no `voice` field) and never runs the prosody
/// computation. A future change that wires voice into the collapse path
/// would break this test.
#[tokio::test]
async fn collapse_path_omits_voice_while_retain_path_attaches_it() {
    fn word_bearing_response() -> SttResponse {
        SttResponse {
            text: "hello world".into(),
            model: SttModelId::new("stub-stt"),
            segments: vec![segment_with_words(vec![
                word("hello", 0.0, 0.5),
                word("world", 2.0, 2.5), // 1.5 s gap => voice on retain path
            ])],
            audio_events: Vec::new(),
            language: None,
        }
    }

    // Retain path (store wired): Transcript block carries VoiceSignals.
    let (retain_next, retain_received) = recording();
    let retain_inbox = SttInbox::new(
        Arc::new(FixedSttProvider::once(word_bearing_response())),
        retain_next,
    )
    .with_audio_store(Arc::new(SucceedingStore))
    .with_policy(Arc::new(AllowAllPolicy));
    retain_inbox
        .receive(event_with(audio_block()))
        .await
        .expect("receive succeeded");
    {
        let guard = retain_received
            .lock()
            .expect("mutex should not be poisoned");
        let ContentBlock::Transcript { voice, .. } = &guard[0].attachments[0] else {
            panic!(
                "retain path should yield a Transcript block, got {:?}",
                &guard[0].attachments[0]
            );
        };
        assert!(
            voice.is_some(),
            "retain path must attach VoiceSignals when timing words are present"
        );
    }

    // Collapse path (no store): folds to a plain Text block, no voice.
    let (collapse_next, collapse_received) = recording();
    let collapse_inbox = SttInbox::new(
        Arc::new(FixedSttProvider::once(word_bearing_response())),
        collapse_next,
    )
    .with_policy(Arc::new(AllowAllPolicy));
    collapse_inbox
        .receive(event_with(audio_block()))
        .await
        .expect("receive succeeded");
    {
        let guard = collapse_received
            .lock()
            .expect("mutex should not be poisoned");
        assert!(
            matches!(&guard[0].attachments[0], ContentBlock::Text { .. }),
            "collapse path should fold transcripts into a Text block, got {:?}",
            &guard[0].attachments[0]
        );
        assert!(
            !guard[0]
                .attachments
                .iter()
                .any(|block| matches!(block, ContentBlock::Transcript { .. })),
            "collapse path must not emit a Transcript block (no voice attachment)"
        );
    }
}

/// A response with only `audio_events` (no segments/words) should still
/// produce `voice = Some` with a non-empty `audio_events` list and
/// `pause_ms = None` / `speech_rate_wpm = None` (no timing data fabricated).
#[tokio::test]
async fn voice_events_only_when_no_words() {
    use crabgent_core::AudioEvent;

    let response = SttResponse {
        text: "ha".into(),
        model: SttModelId::new("stub-stt"),
        segments: Vec::new(),
        audio_events: vec![AudioEvent::new("laughter")],
        language: None,
    };

    let provider = Arc::new(FixedSttProvider::once(response));
    let (next, received) = recording();
    let inbox = SttInbox::new(provider, next)
        .with_audio_store(Arc::new(SucceedingStore))
        .with_policy(Arc::new(AllowAllPolicy));

    inbox
        .receive(event_with(audio_block()))
        .await
        .expect("receive succeeded");

    let guard = received.lock().expect("mutex should not be poisoned");
    let ContentBlock::Transcript { voice, .. } = &guard[0].attachments[0] else {
        panic!(
            "expected Transcript block, got {:?}",
            &guard[0].attachments[0]
        );
    };
    let signals = voice
        .as_ref()
        .expect("VoiceSignals should be Some when audio_events are present");
    assert!(
        !signals.audio_events.is_empty(),
        "audio_events should be forwarded from the SttResponse"
    );
    assert!(
        signals.pause_ms.is_none(),
        "pause_ms should be None when no words are present (no fabrication)"
    );
    assert!(
        signals.speech_rate_wpm.is_none(),
        "speech_rate_wpm should be None when no words are present (no fabrication)"
    );
}

/// A plain text-only response (no words, no audio events) should yield
/// `voice = None` rather than an empty `VoiceSignals`.
#[tokio::test]
async fn voice_none_when_no_signal() {
    let response = SttResponse {
        text: "ok".into(),
        model: SttModelId::new("stub-stt"),
        segments: Vec::new(),
        audio_events: Vec::new(),
        language: None,
    };

    let provider = Arc::new(FixedSttProvider::once(response));
    let (next, received) = recording();
    let inbox = SttInbox::new(provider, next)
        .with_audio_store(Arc::new(SucceedingStore))
        .with_policy(Arc::new(AllowAllPolicy));

    inbox
        .receive(event_with(audio_block()))
        .await
        .expect("receive succeeded");

    let guard = received.lock().expect("mutex should not be poisoned");
    let ContentBlock::Transcript { voice, .. } = &guard[0].attachments[0] else {
        panic!(
            "expected Transcript block, got {:?}",
            &guard[0].attachments[0]
        );
    };
    assert!(
        voice.is_none(),
        "voice should be None when response carries no signal at all"
    );
}
