use std::sync::Arc;
use std::time::Duration;

use crabgent_channel_slack::connection;
use crabgent_channel_slack::dispatch::ListenerRegistry;
use crabgent_channel_slack::events::SocketModeEnvelope;
use crabgent_channel_slack::socket_mode::SocketModeClient;
use crabgent_channel_slack::socket_mode_mock::MockSocketModeClient;
use serde_json::json;

#[tokio::test]
async fn slow_ack_times_out_and_reacks() {
    let mock = MockSocketModeClient::new();
    mock.set_ack_delay(Duration::from_millis(3_100)).await;
    let socket: Arc<dyn SocketModeClient> = mock.clone();
    let envelope: SocketModeEnvelope = serde_json::from_value(json!({
        "type": "events_api",
        "envelope_id": "E-timeout",
        "payload": {"event": {"type": "unknown_event"}}
    }))
    .expect("envelope");

    connection::handle_envelope(
        socket,
        Arc::new(ListenerRegistry::new()),
        envelope,
        &connection::DispatchCtx::single(),
    )
    .await
    .expect("timeout path should continue");

    tokio::time::timeout(Duration::from_secs(5), mock.wait_for_ack_count(2))
        .await
        .expect("re-ACK");
    mock.assert_ack("E-timeout").await;
}
