use super::*;
use crate::channel::TelegramChannel;
use async_trait::async_trait;
use crabgent_channel::image_store::file_system::{
    FileSystemImageStore, FileSystemImageStoreConfig,
};
use httpmock::Method::POST;
use httpmock::MockServer;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tracing_test::traced_test;

#[derive(Clone, Copy)]
enum InboxOutcome {
    Ok,
    AdapterErr,
    ShuttingDown,
}

struct ScriptedInbox {
    events: Mutex<Vec<InboundEvent>>,
    attempts: Mutex<Vec<String>>,
    outcomes: Mutex<VecDeque<InboxOutcome>>,
}

impl ScriptedInbox {
    fn new() -> Self {
        Self::with_outcomes([])
    }

    fn with_outcomes(outcomes: impl IntoIterator<Item = InboxOutcome>) -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            attempts: Mutex::new(Vec::new()),
            outcomes: Mutex::new(outcomes.into_iter().collect()),
        }
    }

    fn drain(&self) -> Vec<InboundEvent> {
        std::mem::take(&mut *self.events.lock().expect("mutex should not be poisoned"))
    }

    fn attempts(&self) -> Vec<String> {
        self.attempts
            .lock()
            .expect("mutex should not be poisoned")
            .clone()
    }

    fn next_outcome(&self) -> InboxOutcome {
        self.outcomes
            .lock()
            .expect("test result")
            .pop_front()
            .unwrap_or(InboxOutcome::Ok)
    }
}

#[async_trait]
impl ChannelInbox for ScriptedInbox {
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        self.attempts
            .lock()
            .expect("mutex should not be poisoned")
            .push(event.message.id.clone());
        match self.next_outcome() {
            InboxOutcome::Ok => {
                self.events
                    .lock()
                    .expect("mutex should not be poisoned")
                    .push(event);
                Ok(())
            }
            InboxOutcome::AdapterErr => Err(ChannelError::adapter("inbox failed")),
            InboxOutcome::ShuttingDown => Err(ChannelError::ShuttingDown),
        }
    }
}

struct NoopInbox;

#[async_trait]
impl ChannelInbox for NoopInbox {
    async fn receive(&self, _event: InboundEvent) -> Result<(), ChannelError> {
        Ok(())
    }
}

fn inbox_obj(inbox: &Arc<ScriptedInbox>) -> Arc<dyn ChannelInbox> {
    inbox.clone()
}

fn build_poller(server: &MockServer, inbox: Arc<dyn ChannelInbox>) -> TelegramPoller {
    let channel = Arc::new(
        TelegramChannel::new("tk", "B-1", "crabgent_bot").with_api_base(server.base_url()),
    );
    TelegramPoller::new(channel, inbox)
        .with_poll_timeout(Duration::from_millis(50))
        .with_error_backoff(Duration::from_millis(50))
}

fn poller_without_image_support() -> TelegramPoller {
    let channel = Arc::new(TelegramChannel::new("secret", "B-1", "nova"));
    TelegramPoller::new(channel, Arc::new(NoopInbox))
}

#[test]
fn build_update_json_ignores_missing_text() {
    let value = build_update_json(1, 1, 1, "", "private");
    let update: TelegramUpdate = serde_json::from_value(value).expect("build update");
    assert!(update.message.is_some());
}

#[tokio::test]
async fn private_text_message_dispatches_event() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [build_update_json(1, 42, 7, "hi", "private")]
        }));
    });
    let inbox = Arc::new(ScriptedInbox::new());
    let mut poller = build_poller(&server, inbox_obj(&inbox));

    poller.tick_once().await.expect("test result");

    let events = inbox.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].body, "hi");
    assert_eq!(events[0].conv.as_str(), "telegram:42");
    assert_eq!(events[0].from.id.as_str(), "7");
    assert_eq!(events[0].message.id, "1");
}

#[tokio::test]
async fn group_message_is_filtered() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [build_update_json(1, 42, 7, "hi", "group")]
        }));
    });
    let inbox = Arc::new(ScriptedInbox::new());
    let mut poller = build_poller(&server, inbox_obj(&inbox));

    poller.tick_once().await.expect("test result");

    assert!(inbox.drain().is_empty());
    assert_eq!(poller.last_offset, Some(1));
}

#[tokio::test]
async fn non_text_message_is_filtered() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [{
                "update_id": 5,
                "message": {
                    "message_id": 5,
                    "date": 1_700_000_000,
                    "chat": {"id": 42, "type": "private"},
                    "from": {"id": 7}
                }
            }]
        }));
    });
    let inbox = Arc::new(ScriptedInbox::new());
    let mut poller = build_poller(&server, inbox_obj(&inbox));

    poller.tick_once().await.expect("test result");

    assert!(inbox.drain().is_empty());
    assert_eq!(poller.last_offset, Some(5));
}

#[tokio::test]
async fn forum_topic_message_carries_thread_root() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [{
                "update_id": 9,
                "message": {
                    "message_id": 9,
                    "date": 1_700_000_000,
                    "chat": {"id": 42, "type": "private"},
                    "from": {"id": 7, "username": "alice"},
                    "text": "hi",
                    "message_thread_id": 42
                }
            }]
        }));
    });
    let inbox = Arc::new(ScriptedInbox::new());
    let mut poller = build_poller(&server, inbox_obj(&inbox));

    poller.tick_once().await.expect("test result");

    let events = inbox.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].message.thread_root.as_deref(), Some("42"));
    assert!(events[0].message.is_thread_reply());
}

#[tokio::test]
async fn last_offset_advances_after_successful_receive() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [
                build_update_json(7, 42, 7, "a", "private"),
                build_update_json(11, 42, 7, "b", "private")
            ]
        }));
    });
    let inbox = Arc::new(ScriptedInbox::new());
    let mut poller = build_poller(&server, inbox_obj(&inbox));

    poller.tick_once().await.expect("test result");

    assert_eq!(poller.last_offset, Some(11));
}

#[tokio::test]
async fn inbox_error_preserves_offset_for_retry() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [build_update_json(7, 42, 7, "retry", "private")]
        }));
    });
    let inbox = Arc::new(ScriptedInbox::with_outcomes([
        InboxOutcome::AdapterErr,
        InboxOutcome::Ok,
    ]));
    let mut poller = build_poller(&server, inbox_obj(&inbox));

    let err = poller.tick_once().await.expect_err("inbox failure");

    assert!(matches!(err, ChannelError::Adapter(_)));
    assert_eq!(poller.last_offset, None);
    assert_eq!(inbox.attempts(), vec!["7".to_owned()]);

    poller.tick_once().await.expect("test result");

    assert_eq!(poller.last_offset, Some(7));
    assert_eq!(inbox.attempts(), vec!["7".to_owned(), "7".to_owned()]);
    assert_eq!(inbox.drain().len(), 1);
}

#[tokio::test]
async fn batch_partial_failure_preserves_failed_update() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [
                build_update_json(7, 42, 7, "first", "private"),
                build_update_json(11, 42, 7, "retry", "private"),
                build_update_json(13, 42, 7, "later", "private")
            ]
        }));
    });
    let inbox = Arc::new(ScriptedInbox::with_outcomes([
        InboxOutcome::Ok,
        InboxOutcome::AdapterErr,
        InboxOutcome::Ok,
        InboxOutcome::Ok,
    ]));
    let mut poller = build_poller(&server, inbox_obj(&inbox));

    let err = poller.tick_once().await.expect_err("second update fails");

    assert!(matches!(err, ChannelError::Adapter(_)));
    assert_eq!(poller.last_offset, Some(7));
    assert_eq!(inbox.attempts(), vec!["7".to_owned(), "11".to_owned()]);
    assert_eq!(inbox.drain().len(), 1);

    poller.tick_once().await.expect("test result");

    assert_eq!(poller.last_offset, Some(13));
    assert_eq!(
        inbox.attempts(),
        vec![
            "7".to_owned(),
            "11".to_owned(),
            "11".to_owned(),
            "13".to_owned(),
        ]
    );
    assert_eq!(inbox.drain().len(), 2);
}

#[tokio::test]
async fn shutdown_during_tick_preserves_offset() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [build_update_json(17, 42, 7, "stop", "private")]
        }));
    });
    let inbox = Arc::new(ScriptedInbox::with_outcomes([InboxOutcome::ShuttingDown]));
    let mut poller = build_poller(&server, inbox_obj(&inbox));

    let err = poller.tick_once().await.expect_err("shutdown");

    assert!(matches!(err, ChannelError::ShuttingDown));
    assert_eq!(poller.last_offset, None);
    assert_eq!(inbox.attempts(), vec!["17".to_owned()]);
    assert!(inbox.drain().is_empty());
}

#[tokio::test]
async fn fetch_updates_propagates_5xx_as_adapter_error() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(503);
    });
    let inbox = Arc::new(ScriptedInbox::new());
    let mut poller = build_poller(&server, inbox_obj(&inbox));

    let err = poller.tick_once().await.expect_err("fail");

    assert!(matches!(err, ChannelError::Adapter(_)));
}

#[tokio::test]
async fn photo_only_download_failure_uses_fallback_body() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [{
                "update_id": 123,
                "message": {
                    "message_id": 123,
                    "date": 1_700_000_000,
                    "chat": {"id": 42, "type": "private"},
                    "from": {"id": 77, "username": "alice"},
                    "photo": [{"file_id": "f-1", "width": 300, "height": 200}]
                }
            }]
        }));
    });
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getFile");
        then.status(500);
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FileSystemImageStore::new(FileSystemImageStoreConfig {
        cache_root: dir.path().to_owned(),
    }));
    let inbox = Arc::new(ScriptedInbox::new());
    let mut poller = build_poller(&server, inbox_obj(&inbox)).with_image_support(
        reqwest::Client::new(),
        store,
        ImageValidator::new(),
    );

    poller.tick_once().await.expect("test result");

    let events = inbox.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].body, "");
    let [ContentBlock::Text { text }] = events[0].attachments.as_slice() else {
        panic!("photo download failure should produce one text fallback");
    };
    assert_eq!(text, IMAGE_PROCESSING_FALLBACK_BODY);
}

#[tokio::test]
async fn update_to_event_with_photo_no_caption_without_store_is_dropped() {
    let poller = poller_without_image_support();
    let update = TelegramUpdate {
        update_id: 1,
        message: Some(TelegramMessage {
            message_id: 123,
            date: 1_700_000_000,
            chat: TelegramChat {
                id: 42,
                chat_type: TELEGRAM_PRIVATE_TYPE.to_owned(),
            },
            from: Some(TelegramUser {
                id: 77,
                username: Some("alice".into()),
                first_name: None,
                last_name: None,
            }),
            text: None,
            caption: None,
            message_thread_id: None,
            photo: Some(vec![PhotoSize {
                file_id: "f-1".into(),
                width: 300,
                height: 200,
            }]),
            voice: None,
            audio: None,
        }),
        message_reaction: None,
    };

    assert!(poller.update_to_event(&update).await.is_none());
}

#[tokio::test]
async fn update_to_event_skips_empty_non_media_messages() {
    let poller = poller_without_image_support();
    let update = TelegramUpdate {
        update_id: 1,
        message: Some(TelegramMessage {
            message_id: 1,
            date: 1_700_000_000,
            chat: TelegramChat {
                id: 1,
                chat_type: TELEGRAM_PRIVATE_TYPE.to_owned(),
            },
            from: Some(TelegramUser {
                id: 7,
                username: Some("alice".into()),
                first_name: None,
                last_name: None,
            }),
            text: None,
            caption: None,
            message_thread_id: None,
            photo: None,
            voice: None,
            audio: None,
        }),
        message_reaction: None,
    };

    assert!(poller.update_to_event(&update).await.is_none());
}

#[test]
fn display_name_prefers_username() {
    let u = TelegramUser {
        id: 1,
        username: Some("alice".into()),
        first_name: Some("Alice".into()),
        last_name: Some("Smith".into()),
    };
    assert_eq!(display_name(&u).as_deref(), Some("alice"));
}

#[test]
fn display_name_falls_back_to_first_last() {
    let u = TelegramUser {
        id: 1,
        username: None,
        first_name: Some("Alice".into()),
        last_name: Some("Smith".into()),
    };
    assert_eq!(display_name(&u).as_deref(), Some("Alice Smith"));
}

#[test]
fn display_name_returns_none_for_missing_fields() {
    let u = TelegramUser {
        id: 1,
        username: None,
        first_name: None,
        last_name: None,
    };
    assert_eq!(display_name(&u), None);
}

#[traced_test]
#[test]
fn timestamp_to_utc_invalid_warns_and_falls_back_to_now() {
    let now_before = Utc::now();
    let ts = timestamp_to_utc(i64::MAX);
    assert!(logs_contain("invalid update timestamp; using now"));
    assert!(logs_contain("timestamp_raw"));
    assert!(ts >= now_before);
}
