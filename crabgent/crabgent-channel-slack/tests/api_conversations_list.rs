mod common;

use crabgent_channel_slack::ConversationType;
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::slack_test_ctx;

#[tokio::test]
async fn conversations_list_follows_cursor_pagination() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    mount_paginated_list(server).await;
    let client = ctx.http_client_with_retry(2);

    let channels = client
        .conversations_list(&[
            ConversationType::PublicChannel,
            ConversationType::PrivateChannel,
            ConversationType::Im,
        ])
        .await
        .expect("conversations.list should join both pages");

    let ids = channels
        .iter()
        .map(|conv| conv.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["C1", "C2", "D9"]);

    let names = channels
        .iter()
        .map(|conv| conv.name.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(names, [Some("platform-ops"), Some("tech"), None]);
    assert!(channels[2].is_im, "DM page entry keeps the is_im flag");
}

#[tokio::test]
async fn conversations_list_returns_partial_set_on_page_cap() {
    // A buggy server that never clears next_cursor must not spin forever and
    // must not error: conversations_list stays fail-soft, returning the
    // accumulated (truncated) set after CONVERSATIONS_LIST_MAX_PAGES pages.
    // The truncation is surfaced via a warn! log (not assertable here without
    // a tracing subscriber); the observable contract is the bounded Ok set.
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    // Every page returns one channel and a non-empty cursor, so the loop runs
    // until the page cap and exits with the cursor still set.
    Mock::given(method("POST"))
        .and(path("/conversations.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [{"id": "C1", "name": "loop", "is_member": true}],
            "response_metadata": {"next_cursor": "MORE"}
        })))
        .mount(server)
        .await;

    let client = ctx.http_client_with_retry(0);
    let channels = client
        .conversations_list(&[ConversationType::PublicChannel])
        .await
        .expect("page-cap truncation stays fail-soft, returns Ok");

    // CONVERSATIONS_LIST_MAX_PAGES is 100: one channel per page, capped.
    assert_eq!(
        channels.len(),
        100,
        "exactly one channel per page up to the 100-page cap"
    );
}

#[tokio::test]
async fn auth_test_exposes_workspace_team() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    Mock::given(method("POST"))
        .and(path("/auth.test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "user_id": "U0",
            "bot_id": "B0",
            "team": "example"
        })))
        .expect(1)
        .mount(server)
        .await;

    let client = ctx.http_client_with_retry(2);
    let auth = client.auth_test().await.expect("auth.test");
    assert_eq!(auth.team.as_deref(), Some("example"));
}

/// The page-one mock (no cursor matcher) also matches the page-two request,
/// so the page-two mock is keyed on the `cursor=PAGE2` form field AND given
/// the higher priority (lower number wins) so it is selected for the second
/// request. The first request carries no cursor field and only the page-one
/// mock applies.
async fn mount_paginated_list(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/conversations.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [
                {"id": "C1", "name": "platform-ops", "is_member": true},
                {"id": "C2", "name": "tech", "is_private": true, "is_member": true}
            ],
            "response_metadata": {"next_cursor": "PAGE2"}
        })))
        .with_priority(5)
        .expect(1)
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/conversations.list"))
        .and(body_string_contains("cursor=PAGE2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [
                {"id": "D9", "is_im": true}
            ],
            "response_metadata": {"next_cursor": ""}
        })))
        .with_priority(1)
        .expect(1)
        .mount(server)
        .await;
}
