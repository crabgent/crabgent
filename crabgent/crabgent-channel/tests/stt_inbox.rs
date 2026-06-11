//! Core `SttInbox` transcription behaviour: text rendering, ordering, error
//! propagation, and audio-store put-failure fallback. Retention, speaker
//! identification, and policy-gate tests live in `stt_inbox_retain.rs`.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use common::{ScriptedSttProvider, audio, event_with, recording};
use crabgent_channel::{
    AUDIO_TRANSCRIPT_PREFIX, AudioStore, AudioStoreError, ChannelError, ChannelInbox, SttInbox,
};
use crabgent_core::{AllowAllPolicy, AudioRef, ContentBlock, SttError};

struct FailingAudioStore;

#[async_trait]
impl AudioStore for FailingAudioStore {
    async fn put(&self, _bytes: Bytes, _mime: &str) -> Result<AudioRef, AudioStoreError> {
        Err(AudioStoreError::Io {
            source: std::io::Error::other("injected test failure"),
        })
    }

    async fn get(&self, _audio_ref: &AudioRef) -> Result<(Bytes, String), AudioStoreError> {
        Err(AudioStoreError::NotFound)
    }
}

#[tokio::test]
async fn transcribes_single_audio_attachment() {
    let provider = Arc::new(ScriptedSttProvider::responding(&["hello world"]));
    let (next, received) = recording();
    let inbox = SttInbox::new(provider.clone(), next);

    inbox
        .receive(event_with(vec![audio("one.ogg")]))
        .await
        .expect("audio event forwarded");

    let events = received.lock().expect("mutex should not be poisoned");
    assert_eq!(provider.call_count(), 1);
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].attachments,
        vec![ContentBlock::Text {
            text: format!("{AUDIO_TRANSCRIPT_PREFIX}hello world"),
        }]
    );
}

#[tokio::test]
async fn transcribes_multiple_audios_joined_newline() {
    let provider = Arc::new(ScriptedSttProvider::responding(&["first", "second"]));
    let (next, received) = recording();
    let inbox = SttInbox::new(provider.clone(), next);

    inbox
        .receive(event_with(vec![audio("one.ogg"), audio("two.ogg")]))
        .await
        .expect("multi-audio event forwarded");

    let events = received.lock().expect("mutex should not be poisoned");
    assert_eq!(provider.call_count(), 2);
    assert_eq!(
        events[0].attachments,
        vec![ContentBlock::Text {
            text: format!("{AUDIO_TRANSCRIPT_PREFIX}first\n{AUDIO_TRANSCRIPT_PREFIX}second"),
        }]
    );
}

#[tokio::test]
async fn transcribes_interleaved_audio_blocks_preserves_text_order() {
    let provider = Arc::new(ScriptedSttProvider::responding(&["T1", "T2"]));
    let (next, received) = recording();
    let inbox = SttInbox::new(provider.clone(), next);

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
        .expect("interleaved audio event forwarded");

    let events = received.lock().expect("mutex should not be poisoned");
    assert_eq!(provider.call_count(), 2);
    assert_eq!(
        events[0].attachments,
        vec![
            ContentBlock::Text {
                text: "before".into(),
            },
            ContentBlock::Text {
                text: format!("{AUDIO_TRANSCRIPT_PREFIX}T1\n{AUDIO_TRANSCRIPT_PREFIX}T2"),
            },
            ContentBlock::Text {
                text: "between".into(),
            },
        ]
    );
}

#[tokio::test]
async fn propagates_stt_error_as_stt_failed() {
    let provider = Arc::new(ScriptedSttProvider::failing(SttError::Backend(
        "provider unavailable".into(),
    )));
    let (next, received) = recording();
    let inbox = SttInbox::new(provider, next);

    let err = inbox
        .receive(event_with(vec![audio("one.ogg")]))
        .await
        .expect_err("STT error propagated");

    assert!(matches!(err, ChannelError::SttFailed(msg) if msg.contains("provider unavailable")));
    assert!(
        received
            .lock()
            .expect("mutex should not be poisoned")
            .is_empty()
    );
}

#[tokio::test]
async fn passes_non_audio_through_unchanged() {
    let provider = Arc::new(ScriptedSttProvider::responding(&["unused"]));
    let (next, received) = recording();
    let inbox = SttInbox::new(provider.clone(), next);
    let attachments = vec![ContentBlock::Text {
        text: "plain attachment".into(),
    }];

    inbox
        .receive(event_with(attachments.clone()))
        .await
        .expect("non-audio event forwarded");

    let events = received.lock().expect("mutex should not be poisoned");
    assert_eq!(provider.call_count(), 0);
    assert_eq!(events[0].body, "voice note");
    assert_eq!(events[0].attachments, attachments);
}

#[tokio::test]
async fn audio_store_put_failure_falls_back_to_text() {
    let provider = Arc::new(ScriptedSttProvider::responding(&["ja super"]));
    let (next, received) = recording();
    let inbox = SttInbox::new(provider.clone(), next)
        .with_audio_store(Arc::new(FailingAudioStore))
        .with_policy(Arc::new(AllowAllPolicy));

    inbox
        .receive(event_with(vec![audio("note.ogg")]))
        .await
        .expect("audio event forwarded despite store failure");

    let events = received.lock().expect("mutex should not be poisoned");
    assert_eq!(provider.call_count(), 1);
    assert_eq!(
        events[0].attachments,
        vec![ContentBlock::Text {
            text: format!("{AUDIO_TRANSCRIPT_PREFIX}ja super"),
        }]
    );
}
