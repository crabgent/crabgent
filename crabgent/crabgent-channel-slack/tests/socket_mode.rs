use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use crabgent_channel_slack::connection::SocketModeKeepAlive;
use crabgent_channel_slack::dispatch::{ListenerRegistry, SlackEventListener};
use crabgent_channel_slack::events::{SlackEvent, SocketModeEnvelope};
use crabgent_channel_slack::socket_mode::{SocketModeClient, SocketModeFrame};
use crabgent_channel_slack::socket_mode_mock::MockSocketModeClient;
use crabgent_channel_slack::{SlackError, connection};
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn mock_socket_mode_parses_envelope_and_acks_fast() {
    let envelope: SocketModeEnvelope = serde_json::from_value(json!({
        "type": "events_api",
        "envelope_id": "E1",
        "payload": {
            "event": {
                "type": "app_mention",
                "team": "T123",
                "channel": "C123",
                "user": "U123",
                "text": "<@BOT> hi",
                "ts": "1.2"
            }
        }
    }))
    .expect("envelope");
    assert!(matches!(
        envelope.event().expect("event"),
        Some(SlackEvent::AppMention(_))
    ));

    let listener = Arc::new(NotifyListener::default());
    let registry = Arc::new(ListenerRegistry::new());
    registry.register(Arc::clone(&listener) as Arc<dyn SlackEventListener>);
    let mock = MockSocketModeClient::new();
    let socket: Arc<dyn SocketModeClient> = mock.clone();

    let started = Instant::now();
    connection::handle_envelope(
        socket,
        registry,
        envelope,
        &connection::DispatchCtx::single(),
    )
    .await
    .expect("handle envelope");

    assert!(started.elapsed() < Duration::from_millis(100));
    mock.assert_ack("E1").await;
    tokio::time::timeout(Duration::from_secs(1), listener.done.notified())
        .await
        .expect("listener recorded envelope");
    assert!(listener.received.load(Ordering::SeqCst));
}

#[tokio::test]
async fn socket_loop_reads_next_envelope_while_listener_is_running() {
    let slow_listener_started = Arc::new(Notify::new());
    let release_slow_listener = CancellationToken::new();
    let registry = Arc::new(ListenerRegistry::new());
    registry.register(Arc::new(SlowListener {
        started: Arc::clone(&slow_listener_started),
        release: release_slow_listener.clone(),
    }) as Arc<dyn SlackEventListener>);
    let mock = MockSocketModeClient::new();
    mock.push_envelope(event_envelope("E1", "1.1")).await;
    mock.push_envelope(event_envelope("E2", "1.2")).await;
    mock.push_error(SlackError::Internal("stop".to_owned()))
        .await;
    let socket: Arc<dyn SocketModeClient> = mock.clone();

    let loop_task = tokio::spawn(connection::socket_message_loop(
        socket,
        registry,
        CancellationToken::new(),
        connection::DispatchCtx::single(),
    ));

    tokio::time::timeout(Duration::from_secs(1), slow_listener_started.notified())
        .await
        .expect("slow listener started");
    tokio::time::timeout(Duration::from_secs(1), mock.wait_for_ack_count(2))
        .await
        .expect("socket loop read the next envelope while listener was still running");
    let result = tokio::time::timeout(Duration::from_secs(1), loop_task)
        .await
        .expect("socket loop finished")
        .expect("socket loop task joined");
    assert!(matches!(result, Err(SlackError::Internal(message)) if message == "stop"));
    assert_eq!(mock.close_count(), 1);
    release_slow_listener.cancel();
}

#[tokio::test]
async fn socket_loop_sends_ping_and_fails_when_no_frames_arrive() {
    let mock = Arc::new(IdleSocketModeClient::default());
    let socket: Arc<dyn SocketModeClient> = mock.clone();
    let result = connection::socket_message_loop_with_keepalive(
        socket,
        Arc::new(ListenerRegistry::new()),
        CancellationToken::new(),
        SocketModeKeepAlive::new(Duration::from_millis(10), Duration::from_millis(50)),
        connection::DispatchCtx::single(),
    )
    .await
    .expect_err("idle socket should hit read timeout");

    assert!(result.to_string().contains("read timeout"));
    assert!(mock.pinged.load(Ordering::SeqCst));
    assert!(mock.closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn socket_loop_counts_heartbeat_frames_as_liveness() {
    let registry = Arc::new(ListenerRegistry::new());
    let mock = MockSocketModeClient::new();
    mock.push_heartbeat().await;
    mock.push_error(SlackError::Internal("stop".to_owned()))
        .await;
    let socket: Arc<dyn SocketModeClient> = mock.clone();

    let result = connection::socket_message_loop_with_keepalive(
        socket,
        registry,
        CancellationToken::new(),
        SocketModeKeepAlive::new(Duration::from_secs(1), Duration::from_secs(2)),
        connection::DispatchCtx::single(),
    )
    .await
    .expect_err("sentinel error exits loop");

    assert!(result.to_string().contains("stop"));
    assert_eq!(mock.close_count(), 1);
}

#[tokio::test]
async fn socket_loop_reconnects_when_heartbeat_frames_hide_envelope_idle() {
    let mock = Arc::new(HeartbeatThenIdleSocketModeClient::default());
    let socket: Arc<dyn SocketModeClient> = mock.clone();

    let result = connection::socket_message_loop_with_keepalive(
        socket,
        Arc::new(ListenerRegistry::new()),
        CancellationToken::new(),
        SocketModeKeepAlive::new(Duration::from_millis(10), Duration::from_millis(80))
            .with_envelope_idle_timeout(Duration::from_millis(30)),
        connection::DispatchCtx::single(),
    )
    .await
    .expect_err("heartbeat-only socket should hit envelope idle timeout");

    assert!(result.to_string().contains("envelope idle"));
    assert!(mock.sent_heartbeat.load(Ordering::SeqCst));
    assert!(mock.closed.load(Ordering::SeqCst));
}

#[derive(Default)]
struct NotifyListener {
    received: AtomicBool,
    done: Notify,
}

#[async_trait]
impl SlackEventListener for NotifyListener {
    async fn on_event(&self, _event: SlackEvent) -> Result<(), SlackError> {
        self.received.store(true, Ordering::SeqCst);
        self.done.notify_one();
        Ok(())
    }
}

struct SlowListener {
    started: Arc<Notify>,
    release: CancellationToken,
}

#[async_trait]
impl SlackEventListener for SlowListener {
    async fn on_event(&self, _event: SlackEvent) -> Result<(), SlackError> {
        self.started.notify_one();
        self.release.cancelled().await;
        Ok(())
    }
}

#[derive(Default)]
struct IdleSocketModeClient {
    pinged: AtomicBool,
    closed: AtomicBool,
}

#[async_trait]
impl SocketModeClient for IdleSocketModeClient {
    async fn connect(&self, _url: &str) -> Result<(), SlackError> {
        Ok(())
    }

    async fn next_envelope(&self) -> Result<SocketModeEnvelope, SlackError> {
        std::future::pending().await
    }

    async fn ack(&self, _envelope_id: &str) -> Result<(), SlackError> {
        Ok(())
    }

    async fn ping(&self) -> Result<(), SlackError> {
        self.pinged.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn close(&self) -> Result<(), SlackError> {
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Default)]
struct HeartbeatThenIdleSocketModeClient {
    sent_heartbeat: AtomicBool,
    closed: AtomicBool,
}

#[async_trait]
impl SocketModeClient for HeartbeatThenIdleSocketModeClient {
    async fn connect(&self, _url: &str) -> Result<(), SlackError> {
        Ok(())
    }

    async fn next_envelope(&self) -> Result<SocketModeEnvelope, SlackError> {
        std::future::pending().await
    }

    async fn next_frame(&self) -> Result<SocketModeFrame, SlackError> {
        if !self.sent_heartbeat.swap(true, Ordering::SeqCst) {
            return Ok(SocketModeFrame::Heartbeat);
        }
        std::future::pending().await
    }

    async fn ack(&self, _envelope_id: &str) -> Result<(), SlackError> {
        Ok(())
    }

    async fn ping(&self) -> Result<(), SlackError> {
        Ok(())
    }

    async fn close(&self) -> Result<(), SlackError> {
        self.closed.store(true, Ordering::SeqCst);
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
