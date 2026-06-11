//! Tests for `bot_message` filtering via `self_bot_id` and `ParticipantRole::Bot` threading.

#![allow(dead_code, reason = "shared test common has unused helpers")]

use crabgent_channel::image_store::file_system::{
    FileSystemImageStore, FileSystemImageStoreConfig,
};
use crabgent_channel::{AudioValidator, ImageValidator, ParticipantRole};
use crabgent_channel_slack::events::{SlackEvent, SlackMessageEvent};
use crabgent_channel_slack::ids::SlackWorkspaceId;
use crabgent_channel_slack::inbound::{
    new_channel_kind_cache, new_channel_type_cache, slack_event_to_inbound_with_channel_type_cache,
};
use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, ResponseTemplate};

mod common;

use common::slack_test_ctx;

fn workspace() -> SlackWorkspaceId {
    SlackWorkspaceId::new("T123").expect("workspace")
}

fn test_deps() -> (
    reqwest::Client,
    SecretString,
    FileSystemImageStore,
    ImageValidator,
    AudioValidator,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
        cache_root: dir.path().to_owned(),
    });
    (
        reqwest::Client::new(),
        SecretString::new("dummy".into()),
        store,
        ImageValidator::new(),
        AudioValidator::new(),
    )
}

fn dm_message(
    subtype: Option<&str>,
    bot_id: Option<&str>,
    user: Option<&str>,
    text: &str,
) -> SlackMessageEvent {
    SlackMessageEvent {
        channel: "C100".into(),
        channel_type: Some("im".into()),
        user: user.map(String::from),
        bot_id: bot_id.map(String::from),
        subtype: subtype.map(String::from),
        text: Some(text.into()),
        ts: "1234.5678".into(),
        thread_ts: None,
        team_id: None,
        files: None,
    }
}

#[tokio::test]
async fn test_own_bot_message_filtered() {
    let (client, token, store, validator, audio_validator) = test_deps();
    let event = SlackEvent::Message(dm_message(Some("bot_message"), Some("B1"), None, "my echo"));
    let inbound = slack_event_to_inbound_with_channel_type_cache(
        &event,
        &workspace(),
        &new_channel_kind_cache(),
        &new_channel_type_cache(),
        Some("B1"),
        &client,
        &token,
        &store,
        &validator,
        &audio_validator,
    )
    .await;
    assert!(inbound.is_none(), "own bot echo should be filtered");
}

#[tokio::test]
async fn test_other_bot_message_yields_bot_role() {
    let (client, token, store, validator, audio_validator) = test_deps();
    let event = SlackEvent::Message(dm_message(
        Some("bot_message"),
        Some("B2"),
        None,
        "other bot msg",
    ));
    let inbound = slack_event_to_inbound_with_channel_type_cache(
        &event,
        &workspace(),
        &new_channel_kind_cache(),
        &new_channel_type_cache(),
        Some("B1"),
        &client,
        &token,
        &store,
        &validator,
        &audio_validator,
    )
    .await
    .expect("other bot message should pass through");
    assert_eq!(inbound.from.role, ParticipantRole::Bot);
}

#[tokio::test]
async fn test_bot_message_no_self_bot_id_passes() {
    let (client, token, store, validator, audio_validator) = test_deps();
    let event = SlackEvent::Message(dm_message(Some("bot_message"), Some("B1"), None, "bot msg"));
    let inbound = slack_event_to_inbound_with_channel_type_cache(
        &event,
        &workspace(),
        &new_channel_kind_cache(),
        &new_channel_type_cache(),
        None,
        &client,
        &token,
        &store,
        &validator,
        &audio_validator,
    )
    .await
    .expect("bot message should pass when self_bot_id is None");
    assert_eq!(inbound.from.role, ParticipantRole::Bot);
}

#[tokio::test]
async fn test_no_subtype_human_passes() {
    let (client, token, store, validator, audio_validator) = test_deps();
    let event = SlackEvent::Message(dm_message(None, None, Some("U1"), "hello"));
    let inbound = slack_event_to_inbound_with_channel_type_cache(
        &event,
        &workspace(),
        &new_channel_kind_cache(),
        &new_channel_type_cache(),
        Some("B1"),
        &client,
        &token,
        &store,
        &validator,
        &audio_validator,
    )
    .await
    .expect("normal user message should pass");
    assert_eq!(inbound.from.role, ParticipantRole::Human);
}

#[tokio::test]
async fn test_auth_test_extracts_bot_id() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    Mock::given(method("POST"))
        .and(path("/auth.test"))
        .and(header("authorization", "Bearer bot-test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "user_id": "U1",
            "bot_id": "B1",
            "team": "T1"
        })))
        .expect(1)
        .mount(server)
        .await;

    let response = ctx
        .http_client_with_retry(2)
        .auth_test()
        .await
        .expect("auth.test should succeed");

    assert!(response.ok);
    assert_eq!(response.bot_id.as_deref(), Some("B1"));
    assert_eq!(response.user_id.as_deref(), Some("U1"));
}

#[tokio::test]
async fn test_auth_test_failure_returns_error() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };

    Mock::given(method("POST"))
        .and(path("/auth.test"))
        .and(header("authorization", "Bearer bot-test-token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "ok": false,
            "error": "not_authed"
        })))
        .mount(server)
        .await;

    let result = ctx.http_client_with_retry(2).auth_test().await;
    assert!(result.is_err(), "401 should return SlackError");
}
