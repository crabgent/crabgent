//! Hardening design8 prompt-injection: inbound sanitize coverage for the
//! Telegram text and caption paths. Sibling of `mod tests` and
//! `mod tests_audio` so `poller.rs` stays under the 500-line cap.

use super::recording_inbox::{RecordingInbox, inbox_obj};
use super::*;
use crate::channel::TelegramChannel;
use httpmock::Method::POST;
use httpmock::MockServer;
use serde_json::Value;
use std::sync::Arc;

fn build_text_poller(server: &MockServer, inbox: Arc<dyn ChannelInbox>) -> TelegramPoller {
    let channel = Arc::new(
        TelegramChannel::new("tk", "B-1", "crabgent_bot").with_api_base(server.base_url()),
    );
    TelegramPoller::new(channel, inbox)
        .with_poll_timeout(Duration::from_millis(50))
        .with_error_backoff(Duration::from_millis(50))
}

fn text_update(text: &str) -> Value {
    json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "date": 1_700_000_000,
            "chat": {"id": 42, "type": "private"},
            "from": {"id": 7, "username": "alice"},
            "text": text,
        }
    })
}

fn caption_update(caption: &str) -> Value {
    json!({
        "update_id": 1,
        "message": {
            "message_id": 1,
            "date": 1_700_000_000,
            "chat": {"id": 42, "type": "private"},
            "from": {"id": 7, "username": "alice"},
            "caption": caption,
        }
    })
}

#[tokio::test]
async fn telegram_text_strips_control_chars() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [text_update("a\u{0000}b\u{200B}c\u{202E}d")]
        }));
    });

    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_text_poller(&server, inbox_obj(&inbox));
    poller.tick_once().await.expect("test result");

    let events = inbox.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].body, "abcd");
}

#[tokio::test]
async fn telegram_caption_strips_control_chars() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [caption_update("cap\u{0000}tion\u{200B}done")]
        }));
    });

    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_text_poller(&server, inbox_obj(&inbox));
    poller.tick_once().await.expect("test result");

    let events = inbox.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].body, "captiondone");
}

#[tokio::test]
async fn telegram_oversize_text_returns_none() {
    let server = MockServer::start();
    let big = "a".repeat(9000);
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [text_update(&big)]
        }));
    });

    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_text_poller(&server, inbox_obj(&inbox));
    poller.tick_once().await.expect("test result");

    assert!(inbox.drain().is_empty());
}
