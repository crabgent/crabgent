mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, Request, ResponseTemplate};

use common::slack_test_ctx;

#[tokio::test]
async fn retries_once_after_rate_limit_on_write_operation() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let responder_hits = Arc::clone(&hits);

    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(move |_request: &Request| {
            if responder_hits.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "0")
            } else {
                ResponseTemplate::new(200)
                    .set_body_json(json!({"ok": true, "channel": "C123", "ts": "1.1"}))
            }
        })
        .mount(server)
        .await;

    let response = ctx
        .http_client_with_retry(2)
        .post_message(&ctx.test_channel, "retry test", None, false, true)
        .await
        .expect("post_message should succeed after retry");

    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert!(response.ok);
    assert_eq!(response.channel, Some("C123".to_string()));
    assert_eq!(response.ts, Some("1.1".to_string()));
}
