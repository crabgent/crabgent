mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crabgent_channel_slack::connection::{ConnectionBackoff, SocketModeConnection};
use crabgent_channel_slack::dispatch::ListenerRegistry;
use crabgent_channel_slack::socket_mode::SocketModeClient;
use crabgent_channel_slack::socket_mode_mock::MockSocketModeClient;
use crabgent_channel_slack::{SlackError, SlackHttpClient};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, Request, ResponseTemplate};

use common::{mount_socket_mode_open, slack_client, slack_test_ctx};

#[tokio::test]
async fn disconnect_error_reconnects_with_backoff() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_socket_mode_open(server).await;
    let mock = MockSocketModeClient::new();
    mock.push_error(SlackError::Internal("disconnect".into()))
        .await;
    mock.push_error(SlackError::Internal("stop".into())).await;
    let socket: Arc<dyn SocketModeClient> = mock.clone();
    let connection = SocketModeConnection::new(
        slack_client(&ctx),
        socket,
        Arc::new(ListenerRegistry::new()),
    )
    .with_backoff(ConnectionBackoff::new(Duration::ZERO, Duration::ZERO));

    let error = connection
        .run_reconnects(Some(1))
        .await
        .expect_err("second disconnect exits test loop");

    assert!(
        error.to_string().contains("stop"),
        "expected sentinel stop error, got {error:?}"
    );
    assert_eq!(mock.connect_count(), 2);
}

#[tokio::test]
async fn connect_error_reconnects_with_backoff() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_socket_mode_open(server).await;
    let mock = MockSocketModeClient::new();
    mock.push_connect_error(SlackError::Internal("connect failed".into()))
        .await;
    mock.push_error(SlackError::Internal("stop".into())).await;
    let socket: Arc<dyn SocketModeClient> = mock.clone();
    let connection = SocketModeConnection::new(
        slack_client(&ctx),
        socket,
        Arc::new(ListenerRegistry::new()),
    )
    .with_backoff(ConnectionBackoff::new(Duration::ZERO, Duration::ZERO));

    let error = connection
        .run_reconnects(Some(1))
        .await
        .expect_err("second attempt exits test loop");

    assert!(error.to_string().contains("stop"));
    assert_eq!(mock.connect_count(), 2);
}

#[tokio::test]
async fn connection_auth_error_breaks_loop() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_socket_mode_open(server).await;
    let mock = MockSocketModeClient::new();
    mock.push_connect_error(SlackError::Auth).await;
    let socket: Arc<dyn SocketModeClient> = mock.clone();
    let connection = SocketModeConnection::new(
        slack_client(&ctx),
        socket,
        Arc::new(ListenerRegistry::new()),
    )
    .with_backoff(ConnectionBackoff::new(Duration::ZERO, Duration::ZERO));

    let error = tokio::time::timeout(Duration::from_millis(250), connection.run_reconnects(None))
        .await
        .expect("auth failure should return before reconnect loop spins")
        .expect_err("auth failure should propagate")
        .to_string();

    assert_eq!(mock.connect_count(), 1);
    assert!(error.contains("Slack authentication failed"));
}

#[tokio::test]
async fn connection_invalid_token_error_breaks_loop() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_socket_mode_open(server).await;
    let mock = MockSocketModeClient::new();
    mock.push_connect_error(SlackError::InvalidToken).await;
    let socket: Arc<dyn SocketModeClient> = mock.clone();
    let connection = SocketModeConnection::new(
        slack_client(&ctx),
        socket,
        Arc::new(ListenerRegistry::new()),
    )
    .with_backoff(ConnectionBackoff::new(Duration::ZERO, Duration::ZERO));

    let error = tokio::time::timeout(Duration::from_millis(250), connection.run_reconnects(None))
        .await
        .expect("invalid token failure should return before reconnect loop spins")
        .expect_err("invalid token failure should propagate")
        .to_string();

    assert_eq!(mock.connect_count(), 1);
    assert!(error.contains("invalid Slack token"));
}

#[tokio::test]
async fn apps_connections_open_rate_limit_cools_down_before_socket_connect() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let hit_times = Arc::new(Mutex::new(Vec::new()));
    let responder_hits = Arc::clone(&hits);
    let responder_hit_times = Arc::clone(&hit_times);
    Mock::given(method("POST"))
        .and(path("/apps.connections.open"))
        .respond_with(move |_request: &Request| {
            responder_hit_times
                .lock()
                .expect("hit times lock should not be poisoned")
                .push(Instant::now());
            if responder_hits.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "0")
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "ok": true,
                    "url": "wss://mock.socket.test"
                }))
            }
        })
        .mount(server)
        .await;
    let mock = MockSocketModeClient::new();
    mock.push_error(SlackError::Internal("stop".into())).await;
    let socket: Arc<dyn SocketModeClient> = mock.clone();
    let cooldown = Duration::from_millis(50);
    let connection = SocketModeConnection::new(
        slack_client_without_retry(&ctx),
        socket,
        Arc::new(ListenerRegistry::new()),
    )
    .with_rate_limit_cooldown(cooldown);

    let error = tokio::time::timeout(Duration::from_secs(1), connection.run_reconnects(Some(1)))
        .await
        .expect("reconnect should finish after test cooldown")
        .expect_err("sentinel socket error exits loop");

    assert_eq!(hits.load(Ordering::SeqCst), 2);
    let hit_times = hit_times
        .lock()
        .expect("hit times lock should not be poisoned");
    assert_eq!(hit_times.len(), 2);
    let elapsed = hit_times[1].duration_since(hit_times[0]);
    assert!(
        elapsed >= cooldown,
        "expected at least {cooldown:?} between rate-limit retry attempts, got {elapsed:?}"
    );
    assert!(
        error.to_string().contains("stop"),
        "expected sentinel stop error, got {error:?}"
    );
    assert_eq!(mock.connect_count(), 1);
}

#[tokio::test]
async fn repeated_apps_connections_open_rate_limit_counts_against_reconnect_cap() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let responder_hits = Arc::clone(&hits);
    Mock::given(method("POST"))
        .and(path("/apps.connections.open"))
        .respond_with(move |_request: &Request| {
            responder_hits.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(429).insert_header("retry-after", "0")
        })
        .mount(server)
        .await;
    let mock = MockSocketModeClient::new();
    let socket: Arc<dyn SocketModeClient> = mock.clone();
    let connection = SocketModeConnection::new(
        slack_client_without_retry(&ctx),
        socket,
        Arc::new(ListenerRegistry::new()),
    )
    .with_rate_limit_cooldown(Duration::from_millis(50));

    let error = tokio::time::timeout(Duration::from_secs(1), connection.run_reconnects(Some(1)))
        .await
        .expect("reconnect should finish after test cooldown")
        .expect_err("second rate limit exceeds reconnect cap");

    assert!(
        matches!(error, SlackError::RateLimited { .. }),
        "expected second rate limit to exceed reconnect cap, got {error:?}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(mock.connect_count(), 0);
}

#[tokio::test]
async fn apps_connections_open_rate_limit_sleep_stops_on_cancel() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let responder_hits = Arc::clone(&hits);
    Mock::given(method("POST"))
        .and(path("/apps.connections.open"))
        .respond_with(move |_request: &Request| {
            responder_hits.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(429).insert_header("retry-after", "0")
        })
        .mount(server)
        .await;
    let cancel = CancellationToken::new();
    let mock = MockSocketModeClient::new();
    let socket: Arc<dyn SocketModeClient> = mock.clone();
    let connection = SocketModeConnection::new(
        slack_client_without_retry(&ctx),
        socket,
        Arc::new(ListenerRegistry::new()),
    )
    .with_cancel(cancel.clone())
    .with_rate_limit_cooldown(Duration::from_secs(5));

    let task = tokio::spawn(async move { connection.run_reconnects(None).await });
    wait_until(|| hits.load(Ordering::SeqCst) == 1).await;
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_millis(500), task)
        .await
        .expect("cancelled reconnect should join before cooldown elapses")
        .expect("reconnect task should join");

    result.expect("cancelled reconnect should stop cleanly");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(mock.connect_count(), 0);
}

fn slack_client_without_retry(ctx: &common::SlackTestCtx) -> Arc<SlackHttpClient> {
    Arc::new(ctx.http_client_with_retry(0))
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        if condition() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition was not met"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
