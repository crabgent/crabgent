//! HTTP-mock integration tests for Telegram inbound reactions.
//!
//! Drives a real `TelegramPoller` against a mocked `/getUpdates`
//! endpoint and asserts that `message_reaction` updates surface as
//! `InboundReaction` events on a recording inbox.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_channel::{ChannelError, ChannelInbox, InboundEvent, InboundReaction};
use crabgent_channel_telegram::poller::test_helpers::build_reaction_update_json;
use crabgent_channel_telegram::{TelegramChannel, TelegramPoller};
use httpmock::Method::POST;
use httpmock::MockServer;
use serde_json::json;

struct RecordingInbox {
    events: Mutex<Vec<InboundEvent>>,
    reactions: Mutex<Vec<InboundReaction>>,
}

impl RecordingInbox {
    const fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            reactions: Mutex::new(Vec::new()),
        }
    }

    fn drain_reactions(&self) -> Vec<InboundReaction> {
        std::mem::take(&mut *self.reactions.lock().expect("mutex should not be poisoned"))
    }

    fn drain_events(&self) -> Vec<InboundEvent> {
        std::mem::take(&mut *self.events.lock().expect("mutex should not be poisoned"))
    }
}

#[async_trait]
impl ChannelInbox for RecordingInbox {
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        self.events
            .lock()
            .expect("mutex should not be poisoned")
            .push(event);
        Ok(())
    }

    async fn receive_reaction(&self, reaction: InboundReaction) -> Result<(), ChannelError> {
        self.reactions
            .lock()
            .expect("mutex should not be poisoned")
            .push(reaction);
        Ok(())
    }
}

fn build_poller(server: &MockServer, inbox: Arc<dyn ChannelInbox>) -> TelegramPoller {
    let channel = Arc::new(
        TelegramChannel::new("tk", "B-1", "crabgent_bot").with_api_base(server.base_url()),
    );
    TelegramPoller::new(channel, inbox)
        .with_poll_timeout(Duration::from_millis(50))
        .with_error_backoff(Duration::from_millis(50))
}

#[tokio::test]
async fn private_message_reaction_added_dispatches_inbound_reaction() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [build_reaction_update_json(1, 42, 7, 1700, "private", &[], &["🎉"])]
        }));
    });
    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_poller(&server, Arc::clone(&inbox) as Arc<dyn ChannelInbox>);

    poller.tick_once().await.expect("test result");

    let reactions = inbox.drain_reactions();
    assert_eq!(reactions.len(), 1);
    let r = &reactions[0];
    assert_eq!(r.channel, "telegram");
    assert_eq!(r.conv.as_str(), "telegram:42");
    assert_eq!(r.from.id.as_str(), "7");
    assert_eq!(r.emoji, "🎉");
    assert!(r.added);
    assert_eq!(r.parent.id, "1700");
    assert!(inbox.drain_events().is_empty());
}

#[tokio::test]
async fn private_message_reaction_removed_dispatches_added_false() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [build_reaction_update_json(2, 42, 7, 1700, "private", &["🎉"], &[])]
        }));
    });
    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_poller(&server, Arc::clone(&inbox) as Arc<dyn ChannelInbox>);

    poller.tick_once().await.expect("test result");

    let reactions = inbox.drain_reactions();
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0].emoji, "🎉");
    assert!(!reactions[0].added);
}

#[tokio::test]
async fn allowed_updates_includes_message_reaction() {
    // Poller MUST request `message_reaction` in `allowed_updates`,
    // otherwise Telegram silently drops reaction updates server-side.
    // Mock only matches when the JSON body carries the string.
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottk/getUpdates")
            .body_includes(r#""message_reaction""#);
        then.status(200)
            .json_body(json!({"ok": true, "result": []}));
    });
    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_poller(&server, Arc::clone(&inbox) as Arc<dyn ChannelInbox>);

    poller.tick_once().await.expect("test result");

    mock.assert();
}

#[tokio::test]
async fn replace_emoji_emits_two_events_added_and_removed() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [build_reaction_update_json(3, 42, 7, 1700, "private", &["👍"], &["❤"])]
        }));
    });
    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_poller(&server, Arc::clone(&inbox) as Arc<dyn ChannelInbox>);

    poller.tick_once().await.expect("test result");

    let reactions = inbox.drain_reactions();
    assert_eq!(reactions.len(), 2);
    assert!(reactions.iter().any(|r| r.emoji == "❤" && r.added));
    assert!(reactions.iter().any(|r| r.emoji == "👍" && !r.added));
}

#[tokio::test]
async fn group_chat_reaction_is_filtered() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [build_reaction_update_json(4, 42, 7, 1700, "group", &[], &["🎉"])]
        }));
    });
    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_poller(&server, Arc::clone(&inbox) as Arc<dyn ChannelInbox>);

    poller.tick_once().await.expect("test result");

    assert!(inbox.drain_reactions().is_empty());
}

#[tokio::test]
async fn custom_emoji_reaction_is_skipped_with_debug_log() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [{
                "update_id": 5,
                "message_reaction": {
                    "chat": {"id": 42, "type": "private"},
                    "message_id": 1700,
                    "user": {"id": 7, "username": "alice"},
                    "date": 1_700_000_000,
                    "old_reaction": [],
                    "new_reaction": [{"type": "custom_emoji", "custom_emoji_id": "5"}],
                }
            }]
        }));
    });
    let inbox = Arc::new(RecordingInbox::new());
    let mut poller = build_poller(&server, Arc::clone(&inbox) as Arc<dyn ChannelInbox>);

    poller.tick_once().await.expect("test result");

    assert!(inbox.drain_reactions().is_empty());
}
