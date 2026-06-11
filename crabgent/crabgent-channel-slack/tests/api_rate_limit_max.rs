mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crabgent_channel_slack::SlackError;
use wiremock::matchers::{method, path};
use wiremock::{Mock, Request, ResponseTemplate};

use common::slack_test_ctx;

#[tokio::test]
async fn surfaces_rate_limit_after_retry_budget() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let responder_hits = Arc::clone(&hits);

    Mock::given(method("POST"))
        .and(path("/conversations.history"))
        .respond_with(move |_request: &Request| {
            responder_hits.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(429).insert_header("retry-after", "0")
        })
        .mount(server)
        .await;

    let error = ctx
        .http_client_with_retry(2)
        .conversations_history(&ctx.test_channel, 1)
        .await
        .expect_err("rate limit should surface");

    assert_eq!(hits.load(Ordering::SeqCst), 3);
    assert!(matches!(error, SlackError::RateLimited { .. }));
}
