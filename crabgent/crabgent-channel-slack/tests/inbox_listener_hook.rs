mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_channel_slack::SlackError;
use crabgent_channel_slack::connection::{ConnectionBackoff, SocketFactory, SocketModePool};
use crabgent_channel_slack::dispatch::{ListenerRegistry, SlackEventListener};
use crabgent_channel_slack::events::SlackEvent;
use crabgent_channel_slack::inbox::SlackInbox;
use crabgent_channel_slack::socket_mode::SocketModeClient;
use crabgent_channel_slack::socket_mode_mock::MockSocketModeClient;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use common::{mount_socket_mode_open, slack_client, slack_test_ctx};

#[tokio::test]
async fn custom_listener_receives_reaction_added() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_socket_mode_open(server).await;
    let mock = MockSocketModeClient::new();
    mock.push_envelope(
        serde_json::from_value(json!({
            "type": "events_api",
            "envelope_id": "E-react",
            "payload": {
                "event": {
                    "type": "reaction_added",
                    "reaction": "eyes",
                    "user": "U123",
                    "item": {"channel": "C123", "ts": "1.1"}
                }
            }
        }))
        .expect("envelope"),
    )
    .await;
    let cancel = CancellationToken::new();
    let socket: Arc<dyn SocketModeClient> = mock;
    let registry = Arc::new(ListenerRegistry::new());
    let factory: SocketFactory = Arc::new(move || Arc::clone(&socket));
    let pool = Arc::new(
        SocketModePool::new(slack_client(&ctx), factory, Arc::clone(&registry))
            .with_connections(1)
            .with_cancel(cancel.clone())
            .with_backoff(ConnectionBackoff::new(
                Duration::default(),
                Duration::default(),
            )),
    );
    let listener = Arc::new(ReactionListener::default());
    let slack_inbox = SlackInbox::new(
        pool,
        Arc::clone(&registry),
        Arc::new(NoopInbox) as Arc<dyn crabgent_channel::ChannelInbox>,
    );
    slack_inbox.register_listener(Arc::clone(&listener) as Arc<dyn SlackEventListener>);

    let handle = slack_inbox.spawn_run();
    tokio::time::timeout(Duration::from_secs(1), listener.wait())
        .await
        .expect("reaction observed");
    cancel.cancel();
    handle.abort();

    assert!(listener.seen.load(Ordering::SeqCst));
}

#[derive(Default)]
struct ReactionListener {
    seen: AtomicBool,
    notify: tokio::sync::Notify,
}

impl ReactionListener {
    async fn wait(&self) {
        loop {
            if self.seen.load(Ordering::SeqCst) {
                return;
            }
            self.notify.notified().await;
        }
    }
}

#[async_trait]
impl SlackEventListener for ReactionListener {
    async fn on_event(&self, event: SlackEvent) -> Result<(), SlackError> {
        if matches!(event, SlackEvent::ReactionAdded(_)) {
            self.seen.store(true, Ordering::SeqCst);
            self.notify.notify_waiters();
        }
        Ok(())
    }
}

struct NoopInbox;

#[async_trait]
impl crabgent_channel::ChannelInbox for NoopInbox {
    async fn receive(
        &self,
        _event: crabgent_channel::InboundEvent,
    ) -> Result<(), crabgent_channel::ChannelError> {
        Ok(())
    }
}
