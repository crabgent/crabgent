//! Socket Mode connection loop.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::api::SlackHttpClient;
use crate::dispatch::ListenerRegistry;
use crate::error::SlackError;
use crate::events::SocketModeEnvelope;
use crate::socket_mode::{SocketModeClient, SocketModeFrame};

pub mod dedup;
pub mod pool;

pub use dedup::EnvelopeDedup;
pub use pool::{SocketFactory, SocketModePool};

const DEFAULT_SOCKET_MODE_PING_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_SOCKET_MODE_READ_TIMEOUT: Duration = Duration::from_secs(90);
const DEFAULT_SOCKET_MODE_ENVELOPE_IDLE_TIMEOUT: Duration = Duration::from_mins(5);
const DEFAULT_SOCKET_MODE_RATE_LIMIT_COOLDOWN: Duration = Duration::from_mins(1);

#[derive(Debug, Clone, Copy)]
pub struct ConnectionBackoff {
    initial: Duration,
    max: Duration,
}

impl Default for ConnectionBackoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_mins(1),
        }
    }
}

impl ConnectionBackoff {
    #[must_use]
    pub const fn new(initial: Duration, max: Duration) -> Self {
        Self { initial, max }
    }

    fn delay(self, attempt: u32) -> Duration {
        let factor = 1_u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX);
        self.initial.saturating_mul(factor).min(self.max)
    }
}

/// Per-connection dispatch context threaded through the Socket Mode message
/// loop: the `conn_id` for tracing and the [`EnvelopeDedup`] shared across all
/// connections so a redelivery on a sibling connection is dropped.
#[derive(Clone)]
pub struct DispatchCtx {
    conn_id: usize,
    dedup: Arc<EnvelopeDedup>,
}

impl DispatchCtx {
    /// Context for connection `conn_id` sharing `dedup` with its siblings.
    #[must_use]
    pub const fn new(conn_id: usize, dedup: Arc<EnvelopeDedup>) -> Self {
        Self { conn_id, dedup }
    }

    /// Context for a standalone single connection: id 0 with a private dedup.
    #[must_use]
    pub fn single() -> Self {
        Self::new(0, Arc::new(EnvelopeDedup::new()))
    }
}

pub struct SocketModeConnection {
    http: Arc<SlackHttpClient>,
    socket: Arc<dyn SocketModeClient>,
    registry: Arc<ListenerRegistry>,
    cancel: CancellationToken,
    backoff: ConnectionBackoff,
    keepalive: SocketModeKeepAlive,
    rate_limit_cooldown: Duration,
    conn_id: usize,
    dedup: Arc<EnvelopeDedup>,
}

impl SocketModeConnection {
    #[must_use]
    pub fn new(
        http: Arc<SlackHttpClient>,
        socket: Arc<dyn SocketModeClient>,
        registry: Arc<ListenerRegistry>,
    ) -> Self {
        Self {
            http,
            socket,
            registry,
            cancel: CancellationToken::new(),
            backoff: ConnectionBackoff::default(),
            keepalive: SocketModeKeepAlive::default(),
            rate_limit_cooldown: DEFAULT_SOCKET_MODE_RATE_LIMIT_COOLDOWN,
            conn_id: 0,
            dedup: Arc::new(EnvelopeDedup::new()),
        }
    }

    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    #[must_use]
    pub const fn with_backoff(mut self, backoff: ConnectionBackoff) -> Self {
        self.backoff = backoff;
        self
    }

    #[must_use]
    pub const fn with_keepalive(mut self, keepalive: SocketModeKeepAlive) -> Self {
        self.keepalive = keepalive;
        self
    }

    #[must_use]
    pub const fn with_rate_limit_cooldown(mut self, rate_limit_cooldown: Duration) -> Self {
        self.rate_limit_cooldown = rate_limit_cooldown;
        self
    }

    #[must_use]
    pub const fn with_conn_id(mut self, conn_id: usize) -> Self {
        self.conn_id = conn_id;
        self
    }

    #[must_use]
    pub fn with_dedup(mut self, dedup: Arc<EnvelopeDedup>) -> Self {
        self.dedup = dedup;
        self
    }

    #[must_use]
    pub fn http_client(&self) -> Arc<SlackHttpClient> {
        Arc::clone(&self.http)
    }

    pub async fn run(&self) {
        match self.run_reconnects(None).await {
            Ok(()) => {}
            Err(error) => {
                crabgent_log::error!(error = %error, "Slack Socket Mode connection stopped with error");
            }
        }
    }

    pub async fn run_reconnects(&self, max_reconnects: Option<u32>) -> Result<(), SlackError> {
        let mut reconnects = 0_u32;
        loop {
            if self.cancel.is_cancelled() {
                return Ok(());
            }
            match self.connection_once().await {
                Ok(()) => return Ok(()),
                Err(_) if self.cancel.is_cancelled() => return Ok(()),
                Err(error) if matches!(error, SlackError::Auth | SlackError::InvalidToken) => {
                    return Err(error);
                }
                Err(error) => {
                    reconnects = reconnects.saturating_add(1);
                    if max_reconnects.is_some_and(|max| reconnects > max) {
                        return Err(error);
                    }
                    let delay = match &error {
                        SlackError::RateLimited { retry_after } => {
                            let retry_after = *retry_after;
                            let delay = retry_after
                                .unwrap_or(self.rate_limit_cooldown)
                                .max(self.rate_limit_cooldown);
                            crabgent_log::warn!(
                                retry_after_secs = retry_after.map(|duration| duration.as_secs()),
                                cooldown_secs = delay.as_secs(),
                                "Slack Socket Mode URL rate limited, cooling down before reconnect"
                            );
                            delay
                        }
                        _ => self.backoff.delay(reconnects.saturating_sub(1)),
                    };
                    sleep_or_cancel(&self.cancel, delay).await;
                    if self.cancel.is_cancelled() {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn connection_once(&self) -> Result<(), SlackError> {
        let url = open_socket_mode_url(&self.http).await?;
        self.socket.connect(&url).await?;
        // Readiness signal: the WebSocket upgrade has completed and this
        // connection is now in Slack's Socket Mode delivery pool. Consumers
        // (e.g. test harnesses restarting the bot) can wait for this line
        // instead of guessing a fixed settle, which avoids posting into the
        // window where Slack still round-robins events to a just-closed
        // connection.
        crabgent_log::info!(conn_id = self.conn_id, "Slack Socket Mode connected");
        socket_message_loop_with_keepalive(
            Arc::clone(&self.socket),
            Arc::clone(&self.registry),
            self.cancel.clone(),
            self.keepalive,
            DispatchCtx::new(self.conn_id, Arc::clone(&self.dedup)),
        )
        .await
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SocketModeKeepAlive {
    ping_interval: Duration,
    read_timeout: Duration,
    envelope_idle_timeout: Duration,
}

impl Default for SocketModeKeepAlive {
    fn default() -> Self {
        Self {
            ping_interval: DEFAULT_SOCKET_MODE_PING_INTERVAL,
            read_timeout: DEFAULT_SOCKET_MODE_READ_TIMEOUT,
            envelope_idle_timeout: DEFAULT_SOCKET_MODE_ENVELOPE_IDLE_TIMEOUT,
        }
    }
}

impl SocketModeKeepAlive {
    #[must_use]
    pub const fn new(ping_interval: Duration, read_timeout: Duration) -> Self {
        Self {
            ping_interval,
            read_timeout,
            envelope_idle_timeout: DEFAULT_SOCKET_MODE_ENVELOPE_IDLE_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn with_envelope_idle_timeout(mut self, envelope_idle_timeout: Duration) -> Self {
        self.envelope_idle_timeout = envelope_idle_timeout;
        self
    }
}

async fn open_socket_mode_url(http: &SlackHttpClient) -> Result<String, SlackError> {
    Ok(http.apps_connections_open().await?.url)
}

pub async fn socket_message_loop(
    socket: Arc<dyn SocketModeClient>,
    registry: Arc<ListenerRegistry>,
    cancel: CancellationToken,
    ctx: DispatchCtx,
) -> Result<(), SlackError> {
    socket_message_loop_with_keepalive(
        socket,
        registry,
        cancel,
        SocketModeKeepAlive::default(),
        ctx,
    )
    .await
}

pub async fn socket_message_loop_with_keepalive(
    socket: Arc<dyn SocketModeClient>,
    registry: Arc<ListenerRegistry>,
    cancel: CancellationToken,
    keepalive: SocketModeKeepAlive,
    ctx: DispatchCtx,
) -> Result<(), SlackError> {
    let result =
        socket_message_loop_inner(Arc::clone(&socket), registry, cancel, keepalive, ctx).await;
    if let Err(error) = socket.close().await {
        crabgent_log::warn!(error = %error, "Slack Socket Mode close failed");
    }
    result
}

async fn socket_message_loop_inner(
    socket: Arc<dyn SocketModeClient>,
    registry: Arc<ListenerRegistry>,
    cancel: CancellationToken,
    keepalive: SocketModeKeepAlive,
    ctx: DispatchCtx,
) -> Result<(), SlackError> {
    keepalive.validate()?;
    let mut ping_interval = tokio::time::interval(keepalive.ping_interval);
    let _ = ping_interval.tick().await;
    let mut deadlines = SocketModeDeadlines::new(keepalive, tokio::time::Instant::now());
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                return Ok(());
            }
            () = tokio::time::sleep_until(deadlines.read) => return Err(read_timeout_error(keepalive)),
            () = tokio::time::sleep_until(deadlines.envelope) => return Err(envelope_idle_timeout_error(keepalive)),
            frame = socket.next_frame() => {
                handle_socket_frame(&socket, &registry, frame?, keepalive, &mut deadlines, &ctx).await?;
            }
            _ = ping_interval.tick() => ping_socket(&socket).await?,
        }
    }
}

struct SocketModeDeadlines {
    read: tokio::time::Instant,
    envelope: tokio::time::Instant,
}

impl SocketModeDeadlines {
    fn new(keepalive: SocketModeKeepAlive, now: tokio::time::Instant) -> Self {
        Self {
            read: now + keepalive.read_timeout,
            envelope: now + keepalive.envelope_idle_timeout,
        }
    }

    fn record_frame(&mut self, keepalive: SocketModeKeepAlive, now: tokio::time::Instant) {
        self.read = now + keepalive.read_timeout;
    }

    fn record_envelope(&mut self, keepalive: SocketModeKeepAlive, now: tokio::time::Instant) {
        self.envelope = now + keepalive.envelope_idle_timeout;
    }
}

fn read_timeout_error(keepalive: SocketModeKeepAlive) -> SlackError {
    crabgent_log::warn!(
        timeout_secs = keepalive.read_timeout.as_secs(),
        "Slack Socket Mode read timeout, connection likely dead"
    );
    SlackError::Internal(format!(
        "Socket Mode read timeout after {:?}",
        keepalive.read_timeout
    ))
}

fn envelope_idle_timeout_error(keepalive: SocketModeKeepAlive) -> SlackError {
    crabgent_log::warn!(
        timeout_secs = keepalive.envelope_idle_timeout.as_secs(),
        "Slack Socket Mode envelope idle timeout, reconnecting"
    );
    SlackError::Internal(format!(
        "Socket Mode envelope idle timeout after {:?}",
        keepalive.envelope_idle_timeout
    ))
}

async fn handle_socket_frame(
    socket: &Arc<dyn SocketModeClient>,
    registry: &Arc<ListenerRegistry>,
    frame: SocketModeFrame,
    keepalive: SocketModeKeepAlive,
    deadlines: &mut SocketModeDeadlines,
    ctx: &DispatchCtx,
) -> Result<(), SlackError> {
    let now = tokio::time::Instant::now();
    deadlines.record_frame(keepalive, now);
    match frame {
        SocketModeFrame::Envelope(envelope) => {
            deadlines.record_envelope(keepalive, now);
            handle_envelope(Arc::clone(socket), Arc::clone(registry), envelope, ctx).await
        }
        SocketModeFrame::Heartbeat => Ok(()),
    }
}

async fn ping_socket(socket: &Arc<dyn SocketModeClient>) -> Result<(), SlackError> {
    match socket.ping().await {
        Ok(()) => Ok(()),
        Err(error) => {
            crabgent_log::warn!(error = %error, "Slack Socket Mode ping failed");
            Err(error)
        }
    }
}

impl SocketModeKeepAlive {
    fn validate(self) -> Result<(), SlackError> {
        if self.ping_interval.is_zero() {
            return Err(SlackError::Internal(
                "Socket Mode ping interval must be nonzero".into(),
            ));
        }
        if self.read_timeout.is_zero() {
            return Err(SlackError::Internal(
                "Socket Mode read timeout must be nonzero".into(),
            ));
        }
        if self.envelope_idle_timeout.is_zero() {
            return Err(SlackError::Internal(
                "Socket Mode envelope idle timeout must be nonzero".into(),
            ));
        }
        if self.ping_interval >= self.read_timeout {
            return Err(SlackError::Internal(
                "Socket Mode ping interval must be shorter than read timeout".into(),
            ));
        }
        Ok(())
    }
}

async fn sleep_or_cancel(cancel: &CancellationToken, delay: Duration) {
    tokio::select! {
        biased;
        () = cancel.cancelled() => {}
        () = tokio::time::sleep(delay) => {}
    }
}

pub async fn handle_envelope(
    socket: Arc<dyn SocketModeClient>,
    registry: Arc<ListenerRegistry>,
    envelope: SocketModeEnvelope,
    ctx: &DispatchCtx,
) -> Result<(), SlackError> {
    if envelope.envelope_type == "disconnect" {
        return Err(SlackError::Internal("Socket Mode disconnect".into()));
    }
    if let Some(envelope_id) = envelope.envelope_id.as_deref() {
        // ACK first (within the 3s budget) so Slack stops retrying even for a
        // redelivery, then consult the shared dedup. A duplicate already seen
        // on a sibling connection is acked but not dispatched again.
        ack_with_deadline(Arc::clone(&socket), envelope_id).await?;
        if !ctx.dedup.accept(envelope_id) {
            crabgent_log::debug!(
                conn_id = ctx.conn_id,
                envelope_id,
                "Slack Socket Mode duplicate envelope acked, skipping dispatch"
            );
            return Ok(());
        }
    }
    // Socket Mode delivers every dispatchable event with an envelope_id (it is
    // required to ACK). Only control frames such as `hello` lack one, and they
    // carry no `event` payload, so the un-deduped path here dispatches nothing
    // for them. If a future envelope type ever dispatches without an id, it
    // would fan out once per connection and need its own dedup key.
    if let Some(event) = envelope.event()? {
        registry.dispatch(event).await;
    }
    Ok(())
}

async fn ack_with_deadline(
    socket: Arc<dyn SocketModeClient>,
    envelope_id: &str,
) -> Result<(), SlackError> {
    if let Ok(result) = tokio::time::timeout(Duration::from_secs(3), socket.ack(envelope_id)).await
    {
        result
    } else {
        crabgent_log::error!(envelope_id, "Slack Socket Mode ACK exceeded 3s deadline");
        let envelope_id = envelope_id.to_owned();
        tokio::spawn(async move {
            if let Err(error) = socket.ack(&envelope_id).await {
                crabgent_log::warn!(error = %error, "Slack Socket Mode re-ACK failed");
            }
        });
        Ok(())
    }
}
