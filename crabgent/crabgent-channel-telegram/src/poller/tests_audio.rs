use super::recording_inbox::{RecordingInbox, inbox_obj};
use super::*;
use crate::channel::TelegramChannel;
use crabgent_test_support::minimal_ogg_bytes;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;
use serde_json::Value;
use std::sync::Arc;

fn build_audio_poller(server: &MockServer, inbox: Arc<dyn ChannelInbox>) -> TelegramPoller {
    let channel = Arc::new(
        TelegramChannel::new("tk", "B-1", "crabgent_bot").with_api_base(server.base_url()),
    );
    TelegramPoller::new(channel, inbox)
        .with_poll_timeout(Duration::from_millis(50))
        .with_error_backoff(Duration::from_millis(50))
        .with_audio_support(reqwest::Client::new(), AudioValidator::new())
}

fn minimal_mp3_bytes() -> Vec<u8> {
    vec![0xFF, 0xFB, 0x90, 0x44, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
}

fn voice_update(file_id: &str, mime_type: Option<&str>, caption: &str) -> Value {
    json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "date": 1_700_000_000,
            "chat": {"id": 42, "type": "private"},
            "from": {"id": 7, "username": "alice"},
            "caption": caption,
            "voice": {
                "file_id": file_id,
                "mime_type": mime_type,
                "duration": 3,
            }
        }
    })
}

fn voice_update_without_caption(file_id: &str, mime_type: Option<&str>) -> Value {
    json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "date": 1_700_000_000,
            "chat": {"id": 42, "type": "private"},
            "from": {"id": 7, "username": "alice"},
            "voice": {
                "file_id": file_id,
                "mime_type": mime_type,
                "duration": 3,
            }
        }
    })
}

fn audio_update(file_id: &str, mime_type: Option<&str>, file_name: &str, caption: &str) -> Value {
    json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "date": 1_700_000_000,
            "chat": {"id": 42, "type": "private"},
            "from": {"id": 7, "username": "alice"},
            "caption": caption,
            "audio": {
                "file_id": file_id,
                "mime_type": mime_type,
                "duration": 4,
                "file_name": file_name,
            }
        }
    })
}

fn text_blocks_len(event: &InboundEvent) -> usize {
    event
        .attachments
        .iter()
        .filter(|block| matches!(block, ContentBlock::Text { .. }))
        .count()
}

#[tokio::test]
async fn voice_without_caption_is_dropped_when_audio_download_fails() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [voice_update_without_caption("voice-id", Some("audio/ogg"))]
        }));
    });
    let get_file = server.mock(|when, then| {
        when.method(POST).path("/bottk/getFile");
        then.status(500);
    });

    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_audio_poller(&server, inbox_obj(&inbox));

    poller.tick_once().await.expect("test result");

    get_file.assert_calls(1);
    let events = inbox.drain();
    assert!(events.is_empty());
}

#[tokio::test]
async fn maps_voice_message() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [voice_update("voice-id", None, "")]
        }));
    });
    let get_file = server.mock(|when, then| {
        when.method(POST).path("/bottk/getFile");
        then.status(200).json_body(json!({
            "ok": true,
            "result": {"file_path": "voice/voice-id.ogg"},
        }));
    });
    let file_get = server.mock(|when, then| {
        when.method(GET).path("/file/bottk/voice/voice-id.ogg");
        then.status(200)
            .header("content-type", "audio/ogg")
            .body(minimal_ogg_bytes());
    });

    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_audio_poller(&server, inbox_obj(&inbox));

    poller.tick_once().await.expect("test result");

    get_file.assert_calls(1);
    file_get.assert_calls(1);
    let events = inbox.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].body, "");
    assert_eq!(events[0].attachments.len(), 1);
    match &events[0].attachments[0] {
        ContentBlock::Audio(payload) => {
            assert_eq!(payload.mime(), "audio/ogg");
            assert_eq!(payload.filename, None);
            assert_eq!(payload.bytes().as_ref(), minimal_ogg_bytes().as_slice());
        }
        other => panic!("expected audio attachment, got {other:?}"),
    }
}

#[tokio::test]
async fn maps_audio_message() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [audio_update("audio-id", Some("audio/mpeg"), "song.mp3", "")]
        }));
    });
    let get_file = server.mock(|when, then| {
        when.method(POST).path("/bottk/getFile");
        then.status(200).json_body(json!({
            "ok": true,
            "result": {"file_path": "audio/audio-id.mp3"},
        }));
    });
    let file_get = server.mock(|when, then| {
        when.method(GET).path("/file/bottk/audio/audio-id.mp3");
        then.status(200)
            .header("content-type", "application/octet-stream")
            .body(minimal_mp3_bytes());
    });

    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_audio_poller(&server, inbox_obj(&inbox));

    poller.tick_once().await.expect("test result");

    get_file.assert_calls(1);
    file_get.assert_calls(1);
    let events = inbox.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].attachments.len(), 1);
    match &events[0].attachments[0] {
        ContentBlock::Audio(payload) => {
            assert_eq!(payload.mime(), "audio/mpeg");
            assert_eq!(payload.filename.as_deref(), Some("song.mp3"));
            assert_eq!(payload.bytes().as_ref(), minimal_mp3_bytes().as_slice());
        }
        other => panic!("expected audio attachment, got {other:?}"),
    }
}

#[tokio::test]
async fn rejects_oversize_audio() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [voice_update("voice-id", Some("audio/ogg"), "rejected")]
        }));
    });
    let get_file = server.mock(|when, then| {
        when.method(POST).path("/bottk/getFile");
        then.status(200).json_body(json!({
            "ok": true,
            "result": {"file_path": "voice/large.ogg"},
        }));
    });
    let over_limit =
        usize::try_from(crabgent_channel::MAX_AUDIO_BYTES).expect("test size fits usize") + 1;
    let mut bytes = Vec::from(b"OggS");
    bytes.resize(over_limit, 0);
    let file_get = server.mock(|when, then| {
        when.method(GET).path("/file/bottk/voice/large.ogg");
        then.status(200)
            .header("content-type", "audio/ogg")
            .body(bytes);
    });

    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_audio_poller(&server, inbox_obj(&inbox));

    poller.tick_once().await.expect("test result");

    get_file.assert_calls(1);
    file_get.assert_calls(1);
    let events = inbox.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].body, "rejected");
    assert!(events[0].attachments.is_empty());
    assert_eq!(text_blocks_len(&events[0]), 0);
}

#[tokio::test]
async fn rejects_disallowed_mime() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [voice_update("voice-id", Some("audio/midi"), "rejected")]
        }));
    });
    let get_file = server.mock(|when, then| {
        when.method(POST).path("/bottk/getFile");
        then.status(200).json_body(json!({
            "ok": true,
            "result": {"file_path": "voice/voice-id.ogg"},
        }));
    });
    let file_get = server.mock(|when, then| {
        when.method(GET).path("/file/bottk/voice/voice-id.ogg");
        then.status(200)
            .header("content-type", "audio/ogg")
            .body(minimal_ogg_bytes());
    });

    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_audio_poller(&server, inbox_obj(&inbox));

    poller.tick_once().await.expect("test result");

    get_file.assert_calls(1);
    file_get.assert_calls(1);
    let events = inbox.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].body, "rejected");
    assert!(events[0].attachments.is_empty());
    assert_eq!(text_blocks_len(&events[0]), 0);
}
