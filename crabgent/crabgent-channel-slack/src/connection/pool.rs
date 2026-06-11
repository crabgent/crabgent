//! Multi-connection Socket Mode pool.
//!
//! Slack delivers each Socket Mode envelope to every open connection in the
//! delivery pool. A single connection therefore loses events whenever a
//! refresh, reconnect, or load-balancing handoff leaves a gap with no live
//! connection. The pool runs several connections in parallel (default 2), each
//! with its own URL, socket, and reconnect loop, and shares one
//! [`EnvelopeDedup`] so the duplicate copies Slack fans out are dropped before
//! dispatch. When one connection reconnects, the others keep delivering, so
//! there is no gap.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::api::SlackHttpClient;
use crate::dispatch::ListenerRegistry;
use crate::socket_mode::SocketModeClient;

use super::dedup::EnvelopeDedup;
use super::{
    ConnectionBackoff, DEFAULT_SOCKET_MODE_RATE_LIMIT_COOLDOWN, SocketModeConnection,
    SocketModeKeepAlive,
};

/// Default number of parallel Socket Mode connections.
const DEFAULT_CONNECTIONS: usize = 2;
/// Lower bound on the connection count.
const MIN_CONNECTIONS: usize = 1;
/// Upper bound on the connection count. Slack permits up to 10 per app token.
const MAX_CONNECTIONS: usize = 10;

/// Builds one Socket Mode client per connection slot.
///
/// Each slot owns a distinct client: the production factory hands out a fresh
/// tungstenite client per call, tests inject mock sockets. Called once per slot
/// when the pool starts; a slot reuses its client across reconnects.
pub type SocketFactory = Arc<dyn Fn() -> Arc<dyn SocketModeClient> + Send + Sync>;

/// Runs several Socket Mode connections in parallel behind a shared dedup.
pub struct SocketModePool {
    http: Arc<SlackHttpClient>,
    factory: SocketFactory,
    registry: Arc<ListenerRegistry>,
    dedup: Arc<EnvelopeDedup>,
    cancel: CancellationToken,
    connections: usize,
    backoff: ConnectionBackoff,
    keepalive: SocketModeKeepAlive,
    rate_limit_cooldown: Duration,
}

impl SocketModePool {
    /// Create a pool with the default connection count.
    #[must_use]
    pub fn new(
        http: Arc<SlackHttpClient>,
        factory: SocketFactory,
        registry: Arc<ListenerRegistry>,
    ) -> Self {
        Self {
            http,
            factory,
            registry,
            dedup: Arc::new(EnvelopeDedup::new()),
            cancel: CancellationToken::new(),
            connections: DEFAULT_CONNECTIONS,
            backoff: ConnectionBackoff::default(),
            keepalive: SocketModeKeepAlive::default(),
            rate_limit_cooldown: DEFAULT_SOCKET_MODE_RATE_LIMIT_COOLDOWN,
        }
    }

    /// Override the number of parallel connections (clamped to 1..=10 at run).
    #[must_use]
    pub const fn with_connections(mut self, connections: usize) -> Self {
        self.connections = connections;
        self
    }

    /// Override the cancellation token shared by every connection.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Override the reconnect backoff applied per connection.
    #[must_use]
    pub const fn with_backoff(mut self, backoff: ConnectionBackoff) -> Self {
        self.backoff = backoff;
        self
    }

    /// Override the keepalive parameters applied per connection.
    #[must_use]
    pub const fn with_keepalive(mut self, keepalive: SocketModeKeepAlive) -> Self {
        self.keepalive = keepalive;
        self
    }

    /// Override the URL rate-limit cooldown applied per connection.
    #[must_use]
    pub const fn with_rate_limit_cooldown(mut self, rate_limit_cooldown: Duration) -> Self {
        self.rate_limit_cooldown = rate_limit_cooldown;
        self
    }

    /// Shared Slack Web API client, for channel sends wired alongside the pool.
    #[must_use]
    pub fn http_client(&self) -> Arc<SlackHttpClient> {
        Arc::clone(&self.http)
    }

    /// Run all connections until the shared cancellation token fires.
    ///
    /// Each connection reconnects independently, so a disconnect on one leaves
    /// the others delivering and no gap opens in Slack's delivery pool.
    pub async fn run(&self) {
        let count = self.connections.clamp(MIN_CONNECTIONS, MAX_CONNECTIONS);
        crabgent_log::info!(connections = count, "Slack Socket Mode pool starting");
        let mut tasks = JoinSet::new();
        for conn_id in 0..count {
            let connection = self.build_connection(conn_id);
            tasks.spawn(async move { connection.run().await });
        }
        while let Some(joined) = tasks.join_next().await {
            Self::log_pool_task_join(joined);
        }
    }

    fn build_connection(&self, conn_id: usize) -> SocketModeConnection {
        SocketModeConnection::new(
            Arc::clone(&self.http),
            (self.factory)(),
            Arc::clone(&self.registry),
        )
        .with_cancel(self.cancel.clone())
        .with_backoff(self.backoff)
        .with_keepalive(self.keepalive)
        .with_rate_limit_cooldown(self.rate_limit_cooldown)
        .with_conn_id(conn_id)
        .with_dedup(Arc::clone(&self.dedup))
    }

    /// Log a finished pool connection task. A panic is a bug, an abort is
    /// cooperative shutdown; both end one slot while sibling connections keep
    /// delivering, so a single warn-level line carrying the kind is enough.
    fn log_pool_task_join(joined: Result<(), tokio::task::JoinError>) {
        if let Err(error) = joined {
            let kind = if error.is_panic() {
                "panicked"
            } else {
                "aborted"
            };
            crabgent_log::warn!(error = %error, kind, "Slack Socket Mode pool connection task ended");
        }
    }
}
