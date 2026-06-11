//! Test double for Socket Mode.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Mutex, Notify};

use crate::error::SlackError;
use crate::events::SocketModeEnvelope;
use crate::socket_mode::{SocketModeClient, SocketModeFrame};

pub struct MockSocketModeClient {
    frames: Mutex<VecDeque<Result<SocketModeFrame, SlackError>>>,
    connect_errors: Mutex<VecDeque<SlackError>>,
    acks: Mutex<Vec<String>>,
    ack_delay: Mutex<Duration>,
    connects: AtomicUsize,
    pings: AtomicUsize,
    closes: AtomicUsize,
    notify_ack: Notify,
    notify_ping: Notify,
}

impl Default for MockSocketModeClient {
    fn default() -> Self {
        Self {
            frames: Mutex::new(VecDeque::new()),
            connect_errors: Mutex::new(VecDeque::new()),
            acks: Mutex::new(Vec::new()),
            ack_delay: Mutex::new(Duration::ZERO),
            connects: AtomicUsize::new(0),
            pings: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            notify_ack: Notify::new(),
            notify_ping: Notify::new(),
        }
    }
}

impl MockSocketModeClient {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn push_envelope(&self, envelope: SocketModeEnvelope) {
        self.frames
            .lock()
            .await
            .push_back(Ok(SocketModeFrame::Envelope(envelope)));
    }

    pub async fn push_heartbeat(&self) {
        self.frames
            .lock()
            .await
            .push_back(Ok(SocketModeFrame::Heartbeat));
    }

    pub async fn push_error(&self, error: SlackError) {
        self.frames.lock().await.push_back(Err(error));
    }

    pub async fn push_connect_error(&self, error: SlackError) {
        self.connect_errors.lock().await.push_back(error);
    }

    pub async fn set_ack_delay(&self, delay: Duration) {
        *self.ack_delay.lock().await = delay;
    }

    pub async fn assert_ack(&self, envelope_id: &str) {
        let acks = self.acks.lock().await;
        assert!(acks.iter().any(|ack| ack == envelope_id));
    }

    pub async fn ack_count(&self) -> usize {
        self.acks.lock().await.len()
    }

    pub async fn wait_for_ack_count(&self, expected: usize) {
        loop {
            let notified = self.notify_ack.notified();
            if self.ack_count().await >= expected {
                return;
            }
            notified.await;
        }
    }

    pub fn ping_count(&self) -> usize {
        self.pings.load(Ordering::SeqCst)
    }

    pub fn close_count(&self) -> usize {
        self.closes.load(Ordering::SeqCst)
    }

    pub async fn wait_for_ping_count(&self, expected: usize) {
        loop {
            let notified = self.notify_ping.notified();
            if self.ping_count() >= expected {
                return;
            }
            notified.await;
        }
    }

    pub fn connect_count(&self) -> usize {
        self.connects.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SocketModeClient for MockSocketModeClient {
    async fn connect(&self, _url: &str) -> Result<(), SlackError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let connect_error = self.connect_errors.lock().await.pop_front();
        if let Some(error) = connect_error {
            return Err(error);
        }
        Ok(())
    }

    async fn next_envelope(&self) -> Result<SocketModeEnvelope, SlackError> {
        loop {
            if let SocketModeFrame::Envelope(envelope) = self.next_frame().await? {
                return Ok(envelope);
            }
        }
    }

    async fn ack(&self, envelope_id: &str) -> Result<(), SlackError> {
        self.acks.lock().await.push(envelope_id.to_owned());
        self.notify_ack.notify_waiters();
        let delay = *self.ack_delay.lock().await;
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        Ok(())
    }

    async fn next_frame(&self) -> Result<SocketModeFrame, SlackError> {
        self.frames
            .lock()
            .await
            .pop_front()
            .unwrap_or_else(|| Err(SlackError::Internal("mock queue empty".into())))
    }

    async fn ping(&self) -> Result<(), SlackError> {
        self.pings.fetch_add(1, Ordering::SeqCst);
        self.notify_ping.notify_waiters();
        Ok(())
    }

    async fn close(&self) -> Result<(), SlackError> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
