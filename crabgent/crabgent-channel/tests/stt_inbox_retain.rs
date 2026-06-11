//! `SttInbox` audio retention behaviour: store-backed transcript emission,
//! speaker-identity enrichment, block-order preservation, and the
//! fail-closed `AudioRetain` policy gate. Core transcription tests live in
//! `stt_inbox.rs`.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::{ScriptedSttProvider, audio, event_with, recording};
use crabgent_channel::audio_store::file_system::{
    FileSystemAudioStore, FileSystemAudioStoreConfig,
};
use crabgent_channel::{
    AUDIO_TRANSCRIPT_PREFIX, AudioStore, ChannelInbox, SpeakerIdentificationError,
    SpeakerIdentificationRequest, SpeakerIdentifier, SttInbox,
};
use crabgent_core::{
    Action, AllowAllPolicy, ContentBlock, DenyAllPolicy, PolicyDecision, PolicyHook,
    SpeakerIdentity, Subject,
};

struct RecordingSpeakerIdentifier {
    identities: Vec<SpeakerIdentity>,
    seen_subjects: Arc<Mutex<Vec<String>>>,
    fail: bool,
}

#[async_trait]
impl SpeakerIdentifier for RecordingSpeakerIdentifier {
    async fn identify(
        &self,
        req: SpeakerIdentificationRequest,
    ) -> Result<Vec<SpeakerIdentity>, SpeakerIdentificationError> {
        self.seen_subjects
            .lock()
            .expect("mutex should not be poisoned")
            .push(req.subject.id().to_owned());
        if self.fail {
            return Err(SpeakerIdentificationError::Backend(
                "test failure".to_owned(),
            ));
        }
        Ok(self.identities.clone())
    }
}

struct RecordingPolicy {
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl PolicyHook for RecordingPolicy {
    async fn allow(&self, subject: &Subject, action: &Action) -> PolicyDecision {
        if matches!(action, Action::AudioRetain { .. }) {
            self.seen
                .lock()
                .expect("mutex should not be poisoned")
                .push(subject.id().to_owned());
        }
        PolicyDecision::Allow
    }
}

fn store_in(dir: &tempfile::TempDir) -> Arc<FileSystemAudioStore> {
    Arc::new(FileSystemAudioStore::new(FileSystemAudioStoreConfig::new(
        dir.path().to_owned(),
    )))
}

#[tokio::test]
async fn with_audio_store_emits_transcript_and_retains_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_in(&dir);
    let provider = Arc::new(ScriptedSttProvider::responding(&["ja super"]));
    let (next, received) = recording();
    let inbox = SttInbox::new(provider.clone(), next)
        .with_audio_store(store.clone())
        .with_policy(Arc::new(AllowAllPolicy));

    inbox
        .receive(event_with(vec![audio("note.ogg")]))
        .await
        .expect("audio event forwarded");

    let source_audio = {
        let events = received.lock().expect("mutex should not be poisoned");
        assert_eq!(provider.call_count(), 1);
        let ContentBlock::Transcript {
            text,
            source_audio,
            voice,
        } = &events[0].attachments[0]
        else {
            panic!("expected Transcript, got {:?}", events[0].attachments[0]);
        };
        assert_eq!(text, "ja super");
        assert!(voice.is_none());
        source_audio.clone()
    };
    let (data, mime) = store.get(&source_audio).await.expect("retained audio");
    assert_eq!(data.as_ref(), b"a");
    assert_eq!(mime, "audio/ogg");
}

#[tokio::test]
async fn speaker_identifier_enriches_retained_transcript_voice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_in(&dir);
    let provider = Arc::new(ScriptedSttProvider::responding(&["ja super"]));
    let (next, received) = recording();
    let seen_subjects = Arc::new(Mutex::new(Vec::new()));
    let speaker_id = Arc::new(RecordingSpeakerIdentifier {
        identities: vec![
            SpeakerIdentity::new("speaker_a", "voiceprint", 87).with_display("Speaker A"),
        ],
        seen_subjects: Arc::clone(&seen_subjects),
        fail: false,
    });
    let inbox = SttInbox::new(provider.clone(), next)
        .with_audio_store(store)
        .with_policy(Arc::new(AllowAllPolicy))
        .with_speaker_identifier(speaker_id);

    inbox
        .receive(event_with(vec![audio("note.ogg")]))
        .await
        .expect("audio event forwarded");

    let events = received.lock().expect("mutex should not be poisoned");
    let ContentBlock::Transcript { voice, .. } = &events[0].attachments[0] else {
        panic!("expected retained transcript");
    };
    let voice = voice
        .as_ref()
        .expect("speaker identity creates voice signals");
    assert_eq!(voice.speaker_identities[0].id, "speaker_a");
    assert_eq!(voice.speaker_identities[0].confidence, 87);
    assert_eq!(
        seen_subjects
            .lock()
            .expect("mutex should not be poisoned")
            .as_slice(),
        ["U1"]
    );
}

#[tokio::test]
async fn speaker_identifier_failure_keeps_retained_transcript() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_in(&dir);
    let provider = Arc::new(ScriptedSttProvider::responding(&["ja super"]));
    let (next, received) = recording();
    let speaker_id = Arc::new(RecordingSpeakerIdentifier {
        identities: Vec::new(),
        seen_subjects: Arc::new(Mutex::new(Vec::new())),
        fail: true,
    });
    let inbox = SttInbox::new(provider.clone(), next)
        .with_audio_store(store)
        .with_policy(Arc::new(AllowAllPolicy))
        .with_speaker_identifier(speaker_id);

    inbox
        .receive(event_with(vec![audio("note.ogg")]))
        .await
        .expect("audio event forwarded despite speaker-id failure");

    let events = received.lock().expect("mutex should not be poisoned");
    let ContentBlock::Transcript { text, voice, .. } = &events[0].attachments[0] else {
        panic!("expected retained transcript");
    };
    assert_eq!(text, "ja super");
    assert!(voice.is_none());
}

#[tokio::test]
async fn with_audio_store_preserves_block_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_in(&dir);
    let provider = Arc::new(ScriptedSttProvider::responding(&["T1", "T2"]));
    let (next, received) = recording();
    let inbox = SttInbox::new(provider.clone(), next)
        .with_audio_store(store)
        .with_policy(Arc::new(AllowAllPolicy));

    inbox
        .receive(event_with(vec![
            ContentBlock::Text {
                text: "before".into(),
            },
            audio("one.ogg"),
            ContentBlock::Text {
                text: "between".into(),
            },
            audio("two.ogg"),
        ]))
        .await
        .expect("interleaved event forwarded");

    let events = received.lock().expect("mutex should not be poisoned");
    let blocks = &events[0].attachments;
    assert_eq!(blocks.len(), 4);
    assert_eq!(
        blocks[0],
        ContentBlock::Text {
            text: "before".into(),
        }
    );
    assert!(matches!(&blocks[1], ContentBlock::Transcript { text, .. } if text == "T1"));
    assert_eq!(
        blocks[2],
        ContentBlock::Text {
            text: "between".into(),
        }
    );
    assert!(matches!(&blocks[3], ContentBlock::Transcript { text, .. } if text == "T2"));
}

#[tokio::test]
async fn retain_denied_by_policy_collapses_to_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = Arc::new(ScriptedSttProvider::responding(&["ja super"]));
    let (next, received) = recording();
    let inbox = SttInbox::new(provider.clone(), next)
        .with_audio_store(store_in(&dir))
        .with_policy(Arc::new(DenyAllPolicy));

    inbox
        .receive(event_with(vec![audio("note.ogg")]))
        .await
        .expect("event forwarded on deny");

    let events = received.lock().expect("mutex should not be poisoned");
    assert_eq!(provider.call_count(), 1);
    // Fail-closed deny: no Transcript, no retained handle, flat text only.
    assert_eq!(
        events[0].attachments,
        vec![ContentBlock::Text {
            text: format!("{AUDIO_TRANSCRIPT_PREFIX}ja super"),
        }]
    );
}

#[tokio::test]
async fn retain_without_policy_is_fail_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = Arc::new(ScriptedSttProvider::responding(&["ja super"]));
    let (next, received) = recording();
    // Store wired but NO policy: retention must not happen (fail-closed).
    let inbox = SttInbox::new(provider.clone(), next).with_audio_store(store_in(&dir));

    inbox
        .receive(event_with(vec![audio("note.ogg")]))
        .await
        .expect("event forwarded without policy");

    let events = received.lock().expect("mutex should not be poisoned");
    assert_eq!(
        events[0].attachments,
        vec![ContentBlock::Text {
            text: format!("{AUDIO_TRANSCRIPT_PREFIX}ja super"),
        }]
    );
}

#[tokio::test]
async fn retain_gate_keys_on_speaker_not_conversation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = Arc::new(ScriptedSttProvider::responding(&["ja super"]));
    let (next, received) = recording();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let inbox = SttInbox::new(provider.clone(), next)
        .with_audio_store(store_in(&dir))
        .with_policy(Arc::new(RecordingPolicy {
            seen: Arc::clone(&seen),
        }));

    // event_with: from = "U1", conv = "slack:T1/D1".
    inbox
        .receive(event_with(vec![audio("note.ogg")]))
        .await
        .expect("event forwarded on allow");

    let gated = seen.lock().expect("mutex should not be poisoned");
    assert_eq!(gated.as_slice(), ["U1"], "gate keys on the speaker id");
    assert!(!gated.iter().any(|id| id == "slack:T1/D1"));

    let events = received.lock().expect("mutex should not be poisoned");
    assert!(matches!(
        &events[0].attachments[0],
        ContentBlock::Transcript { text, .. } if text == "ja super"
    ));
}
