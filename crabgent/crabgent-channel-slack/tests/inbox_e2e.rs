mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_channel::{ChannelError, ChannelInbox, InboundEvent};
use crabgent_channel_slack::connection::{ConnectionBackoff, SocketFactory, SocketModePool};
use crabgent_channel_slack::dispatch::{KernelInboundForwarder, ListenerRegistry, SlackSelfIds};
use crabgent_channel_slack::ids::SlackWorkspaceId;
use crabgent_channel_slack::inbound::{new_channel_kind_cache, new_channel_type_cache};
use crabgent_channel_slack::inbox::SlackInbox;
use crabgent_channel_slack::socket_mode::SocketModeClient;
use crabgent_channel_slack::socket_mode_mock::MockSocketModeClient;
use secrecy::SecretString;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use common::{mount_socket_mode_open, slack_client, slack_test_ctx};

#[tokio::test]
async fn slack_inbox_forwards_socket_event_to_channel_inbox() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_socket_mode_open(server).await;
    let mock = MockSocketModeClient::new();
    let envelope = serde_json::from_value(json!({
        "type": "events_api",
        "envelope_id": "E1",
        "payload": {
            "event": {
                "type": "app_mention",
                "channel": "C123",
                "user": "U123",
                "text": "hello",
                "ts": "1.1"
            }
        }
    }))
    .expect("envelope");
    mock.push_envelope(envelope).await;
    let cancel = CancellationToken::new();
    let socket: Arc<dyn SocketModeClient> = mock.clone();
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
    let inbox = Arc::new(RecordingInbox::default());
    let slack_inbox = SlackInbox::new(
        Arc::clone(&pool),
        Arc::clone(&registry),
        Arc::clone(&inbox) as Arc<dyn ChannelInbox>,
    );
    slack_inbox.register_listener(Arc::new(
        KernelInboundForwarder::with_hardened_client(
            slack_inbox.inbox(),
            SlackWorkspaceId::new("T123").expect("workspace"),
            new_channel_kind_cache(),
            new_channel_type_cache(),
            SlackSelfIds::default(),
            Duration::from_secs(30),
            SecretString::new("dummy".into()),
            Arc::new(
                crabgent_channel::image_store::file_system::FileSystemImageStore::new(
                    crabgent_channel::image_store::file_system::FileSystemImageStoreConfig {
                        cache_root: std::env::temp_dir(),
                    },
                ),
            ),
            crabgent_channel::ImageValidator::new(),
            crabgent_channel::AudioValidator::new(),
        )
        .expect("hardened media client builds"),
    ));

    let handle = slack_inbox.spawn_run();
    tokio::time::timeout(Duration::from_secs(1), inbox.wait_for(1))
        .await
        .expect("event forwarded");
    cancel.cancel();
    handle.abort();

    let events = inbox.events.lock().expect("events");
    assert_eq!(events[0].body, "hello");
}

#[derive(Default)]
struct RecordingInbox {
    events: Mutex<Vec<InboundEvent>>,
    notify: tokio::sync::Notify,
}

impl RecordingInbox {
    async fn wait_for(&self, expected: usize) {
        loop {
            if self.events.lock().expect("events").len() >= expected {
                return;
            }
            self.notify.notified().await;
        }
    }
}

#[async_trait]
impl ChannelInbox for RecordingInbox {
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        self.events.lock().expect("events").push(event);
        self.notify.notify_waiters();
        Ok(())
    }
}
