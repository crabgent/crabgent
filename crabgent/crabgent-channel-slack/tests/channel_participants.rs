mod common;

use crabgent_channel::Channel;
use crabgent_channel_slack::SlackChannel;
use crabgent_core::owner::Owner;
use crabgent_core::subject::Subject;
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

use common::{slack_client, slack_test_ctx};

#[tokio::test]
async fn participants_load_members_and_user_profiles() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/conversations.members"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true, "members": ["U1", "U2"]
        })))
        .mount(server)
        .await;
    mount_user(server, "U1", "Ada").await;
    mount_user(server, "U2", "Grace").await;
    let channel = SlackChannel::new(slack_client(&ctx));

    let participants = channel
        .participants(&Subject::new("agent"), &Owner::new("slack:T123/C123"))
        .await
        .expect("participants");

    assert_eq!(display_names(participants), ["Ada", "Grace"]);
}

#[tokio::test]
async fn participants_deduplicates_member_ids_before_loading_profiles() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/conversations.members"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true, "members": ["U1", "U1", "U2"]
        })))
        .mount(server)
        .await;
    mount_user_once(server, "U1", "Ada").await;
    mount_user_once(server, "U2", "Grace").await;
    let channel = SlackChannel::new(slack_client(&ctx));

    let participants = channel
        .participants(&Subject::new("agent"), &Owner::new("slack:T123/C123"))
        .await
        .expect("participants");

    assert_eq!(display_names(participants), ["Ada", "Grace"]);
}

#[tokio::test]
async fn participants_skips_failed_user_profile_and_keeps_successes() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/conversations.members"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true, "members": ["U1", "U2"]
        })))
        .mount(server)
        .await;
    mount_user(server, "U1", "Ada").await;
    mount_user_error(server, "U2").await;
    let channel = SlackChannel::new(slack_client(&ctx));

    let participants = channel
        .participants(&Subject::new("agent"), &Owner::new("slack:T123/C123"))
        .await
        .expect("participants");

    assert_eq!(display_names(participants), ["Ada"]);
}

async fn mount_user(server: &wiremock::MockServer, id: &str, name: &str) {
    Mock::given(method("POST"))
        .and(path("/users.info"))
        .and(body_string_contains(id))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "user": {"id": id, "name": name}})),
        )
        .mount(server)
        .await;
}

async fn mount_user_once(server: &wiremock::MockServer, id: &str, name: &str) {
    Mock::given(method("POST"))
        .and(path("/users.info"))
        .and(body_string_contains(id))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "user": {"id": id, "name": name}})),
        )
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_user_error(server: &wiremock::MockServer, id: &str) {
    Mock::given(method("POST"))
        .and(path("/users.info"))
        .and(body_string_contains(id))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": false,
            "error": "user_not_found"
        })))
        .mount(server)
        .await;
}

fn display_names(participants: Vec<crabgent_channel::Participant>) -> Vec<String> {
    let mut names = participants
        .into_iter()
        .filter_map(|participant| participant.display_name)
        .collect::<Vec<_>>();
    names.sort();
    names
}
