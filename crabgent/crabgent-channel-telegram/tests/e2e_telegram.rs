//! End-to-end integration test: `TelegramPoller` -> `PairingInbox`
//! -> inner inbox path.
//!
//! Drives a poller against a mocked Telegram API, lets the
//! `/pair <token>` handshake go through `PairingInbox`, then
//! verifies a follow-up plain message is forwarded to the inner
//! inbox while a parallel message from a different user is bounced.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_channel::{
    Channel, ChannelError, ChannelInbox, ChannelRouter, ChannelSink, InboundEvent,
    MemoryPairingStore, PairingInbox, PairingStore,
};
use crabgent_channel_telegram::poller::build_update_json;
use crabgent_channel_telegram::{TelegramChannel, TelegramPoller};
use httpmock::Method::POST;
use httpmock::MockServer;
use serde_json::json;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

struct CountingInbox {
    events: Mutex<Vec<InboundEvent>>,
}

impl CountingInbox {
    const fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }
    fn count(&self) -> usize {
        self.events
            .lock()
            .expect("mutex should not be poisoned")
            .len()
    }
    fn drain(&self) -> Vec<InboundEvent> {
        std::mem::take(&mut *self.events.lock().expect("mutex should not be poisoned"))
    }
}

#[async_trait]
impl ChannelInbox for CountingInbox {
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        self.events
            .lock()
            .expect("mutex should not be poisoned")
            .push(event);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_then_chat_then_unpaired_user_blocked() {
    let server = MockServer::start();

    // getUpdates returns: U1 sends /pair secret (id=1),
    // then U1 plain "hi" (id=2), then U2 plain "spy" (id=3).
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [
                build_update_json(1, 1001, 1, "/pair secret", "private"),
                build_update_json(2, 1001, 1, "hi", "private"),
                build_update_json(3, 1002, 2, "spy", "private"),
            ]
        }));
    });

    // sendMessage gets called for: paired-success-reply (after /pair),
    // not-paired-reply (for U2 spy attempt). Two calls total.
    let send_mock = server.mock(|when, then| {
        when.method(POST).path("/bottk/sendMessage");
        then.status(200).json_body(json!({
            "ok": true,
            "result": {"message_id": 999, "chat": {"id": 1001}}
        }));
    });

    let channel = Arc::new(
        TelegramChannel::new("tk", "B-1", "crabgent_bot").with_api_base(server.base_url()),
    );
    let store = Arc::new(MemoryPairingStore::new());
    let inner = Arc::new(CountingInbox::new());
    let trait_obj_inner: Arc<dyn ChannelInbox> = Arc::clone(&inner) as _;
    let trait_obj_channel: Arc<dyn Channel> = Arc::clone(&channel) as _;
    let router: Arc<dyn ChannelSink> =
        Arc::new(ChannelRouter::new().with_channel(trait_obj_channel));
    let pairing: Arc<dyn ChannelInbox> = Arc::new(PairingInbox::new(
        Arc::clone(&store) as Arc<dyn PairingStore>,
        trait_obj_inner,
        router,
        "secret",
    ));

    let poller = TelegramPoller::new(channel, pairing)
        .with_poll_timeout(Duration::from_millis(50))
        .with_error_backoff(Duration::from_millis(50));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let handle = tokio::spawn(async move { poller.run(cancel_clone).await });

    // Let one or two ticks run (poll timeout is 50ms).
    sleep(Duration::from_millis(300)).await;
    cancel.cancel();
    handle.await.expect("join").expect("run ok");

    // U1 paired through /pair, plain "hi" forwarded.
    assert!(store.is_paired("1").await.expect("test result"));
    assert!(inner.count() >= 1);
    let events = inner.drain();
    assert!(
        events
            .iter()
            .any(|e| e.body == "hi" && e.from.id.as_str() == "1")
    );

    // U2 stayed unpaired, was not forwarded.
    assert!(!store.is_paired("2").await.expect("test result"));

    // Two sendMessage calls: paired-success + not-paired.
    assert!(send_mock.calls() >= 2);
}
