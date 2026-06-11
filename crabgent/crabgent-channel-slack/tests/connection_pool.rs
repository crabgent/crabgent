mod common;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_channel_slack::SlackError;
use crabgent_channel_slack::connection::{ConnectionBackoff, SocketFactory, SocketModePool};
use crabgent_channel_slack::dispatch::{ListenerRegistry, SlackEventListener};
use crabgent_channel_slack::events::{SlackEvent, SocketModeEnvelope};
use crabgent_channel_slack::socket_mode::SocketModeClient;
use crabgent_channel_slack::socket_mode_mock::MockSocketModeClient;
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use common::{mount_socket_mode_open, slack_client, slack_test_ctx};

/// Slack fans the same envelope out to every open connection. The shared dedup
/// must dispatch it exactly once even though both pool connections receive it.
#[tokio::test]
async fn pool_dedups_same_envelope_across_connections() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_socket_mode_open(server).await;

    let mock0 = MockSocketModeClient::new();
    let mock1 = MockSocketModeClient::new();
    mock0.push_envelope(event_envelope("E1", "1.1")).await;
    mock1.push_envelope(event_envelope("E1", "1.1")).await;

    let listener = Arc::new(CountingListener::default());
    let registry = Arc::new(ListenerRegistry::new());
    registry.register(Arc::clone(&listener) as Arc<dyn SlackEventListener>);

    let socket0: Arc<dyn SocketModeClient> = mock0.clone();
    let socket1: Arc<dyn SocketModeClient> = mock1.clone();
    let cancel = CancellationToken::new();
    let pool = SocketModePool::new(
        slack_client(&ctx),
        ordered_factory(vec![socket0, socket1]),
        registry,
    )
    .with_connections(2)
    .with_cancel(cancel.clone())
    .with_backoff(ConnectionBackoff::new(
        Duration::from_millis(50),
        Duration::from_millis(50),
    ));

    let pool_task = tokio::spawn(async move { pool.run().await });

    tokio::time::timeout(Duration::from_secs(2), mock0.wait_for_ack_count(1))
        .await
        .expect("connection 0 acked the envelope");
    tokio::time::timeout(Duration::from_secs(2), mock1.wait_for_ack_count(1))
        .await
        .expect("connection 1 acked the envelope");
    tokio::time::timeout(Duration::from_secs(2), listener.notify.notified())
        .await
        .expect("the deduped envelope dispatched once");

    assert_eq!(
        listener.count.load(Ordering::SeqCst),
        1,
        "a duplicate across connections dispatched more than once"
    );

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), pool_task)
        .await
        .expect("pool stops on cancel")
        .expect("pool task joins");
}

/// A disconnect on one connection makes only that connection reconnect; the
/// sibling keeps delivering, so no event-loss gap opens.
#[tokio::test]
async fn pool_disconnect_on_one_connection_keeps_siblings_delivering() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_socket_mode_open(server).await;

    let mock0 = MockSocketModeClient::new();
    let mock1 = MockSocketModeClient::new();
    mock0.push_envelope(disconnect_envelope()).await;
    mock1.push_envelope(event_envelope("E2", "2.2")).await;

    let listener = Arc::new(NotifyListener::default());
    let registry = Arc::new(ListenerRegistry::new());
    registry.register(Arc::clone(&listener) as Arc<dyn SlackEventListener>);

    let socket0: Arc<dyn SocketModeClient> = mock0.clone();
    let socket1: Arc<dyn SocketModeClient> = mock1.clone();
    let cancel = CancellationToken::new();
    let pool = SocketModePool::new(
        slack_client(&ctx),
        ordered_factory(vec![socket0, socket1]),
        registry,
    )
    .with_connections(2)
    .with_cancel(cancel.clone())
    .with_backoff(ConnectionBackoff::new(
        Duration::from_millis(50),
        Duration::from_millis(50),
    ));

    let pool_task = tokio::spawn(async move { pool.run().await });

    tokio::time::timeout(Duration::from_secs(2), listener.notify.notified())
        .await
        .expect("sibling connection delivered its event during the disconnect");
    assert!(listener.received.load(Ordering::SeqCst));

    wait_until(|| mock0.connect_count() >= 2).await;

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), pool_task)
        .await
        .expect("pool stops on cancel")
        .expect("pool task joins");
}

#[derive(Default)]
struct CountingListener {
    count: AtomicUsize,
    notify: Notify,
}

#[async_trait]
impl SlackEventListener for CountingListener {
    async fn on_event(&self, _event: SlackEvent) -> Result<(), SlackError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_one();
        Ok(())
    }
}

#[derive(Default)]
struct NotifyListener {
    received: AtomicBool,
    notify: Notify,
}

#[async_trait]
impl SlackEventListener for NotifyListener {
    async fn on_event(&self, _event: SlackEvent) -> Result<(), SlackError> {
        self.received.store(true, Ordering::SeqCst);
        self.notify.notify_one();
        Ok(())
    }
}

/// Hands a pre-built socket to each connection slot in order. The pool calls
/// the factory once per slot at startup; a slot reuses its socket across
/// reconnects, so the queue is normally drained exactly to its length. The
/// idle fallback only guards an unexpected extra call.
fn ordered_factory(mocks: Vec<Arc<dyn SocketModeClient>>) -> SocketFactory {
    let queue = Arc::new(Mutex::new(VecDeque::from(mocks)));
    let idle: Arc<dyn SocketModeClient> = Arc::new(IdleSocket);
    Arc::new(move || {
        queue
            .lock()
            .expect("factory queue lock not poisoned")
            .pop_front()
            .unwrap_or_else(|| Arc::clone(&idle))
    })
}

struct IdleSocket;

#[async_trait]
impl SocketModeClient for IdleSocket {
    async fn connect(&self, _url: &str) -> Result<(), SlackError> {
        Ok(())
    }

    async fn next_envelope(&self) -> Result<SocketModeEnvelope, SlackError> {
        std::future::pending().await
    }

    async fn ack(&self, _envelope_id: &str) -> Result<(), SlackError> {
        Ok(())
    }
}

fn event_envelope(envelope_id: &str, ts: &str) -> SocketModeEnvelope {
    serde_json::from_value(json!({
        "type": "events_api",
        "envelope_id": envelope_id,
        "payload": {
            "event": {
                "type": "app_mention",
                "team": "T123",
                "channel": "C123",
                "user": "U123",
                "text": "<@BOT> hi",
                "ts": ts
            }
        }
    }))
    .expect("socket mode event envelope")
}

fn disconnect_envelope() -> SocketModeEnvelope {
    serde_json::from_value(json!({
        "type": "disconnect",
        "reason": "refresh_requested"
    }))
    .expect("socket mode disconnect envelope")
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if condition() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition was not met before the deadline"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
