mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crabgent_channel_slack::SlackError;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, Request, ResponseTemplate};

use common::slack_test_ctx;

#[tokio::test]
async fn apps_connections_open_returns_socket_url() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    assert!(!ctx.test_channel.is_empty());

    Mock::given(method("POST"))
        .and(path("/apps.connections.open"))
        .and(header("authorization", "Bearer app-test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "url": "wss://example.test/socket"
        })))
        .expect(1)
        .mount(server)
        .await;

    let response = ctx
        .http_client_with_retry(2)
        .apps_connections_open()
        .await
        .expect("socket URL should decode");

    assert_eq!(response.url, "wss://example.test/socket");
}

#[tokio::test]
async fn apps_connections_open_surfaces_rate_limit_without_retry() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let responder_hits = Arc::clone(&hits);

    Mock::given(method("POST"))
        .and(path("/apps.connections.open"))
        .and(header("authorization", "Bearer app-test-token"))
        .respond_with(move |_request: &Request| {
            if responder_hits.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "0")
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "ok": true,
                    "url": "wss://retry.socket.test"
                }))
            }
        })
        .mount(server)
        .await;

    let error = ctx
        .http_client_with_retry(2)
        .apps_connections_open()
        .await
        .expect_err("Socket Mode reconnect loop owns apps.connections.open cooldown");

    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(matches!(
        error,
        SlackError::RateLimited {
            retry_after: Some(delay)
        } if delay == Duration::ZERO
    ));
}
