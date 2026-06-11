mod common;

use std::sync::Arc;

use crabgent_channel::{ChannelInbox, InboundEvent};
use crabgent_channel_slack::SlackChannelId;
use crabgent_channel_slack::connection::{SocketFactory, SocketModePool};
use crabgent_channel_slack::dispatch::ListenerRegistry;
use crabgent_channel_slack::inbox::SlackInbox;
use crabgent_channel_slack::socket_mode::SocketModeClient;
use crabgent_channel_slack::socket_mode_mock::MockSocketModeClient;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{slack_client, slack_test_ctx};

#[tokio::test]
async fn pre_warm_fills_name_map_and_workspace() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_list(server).await;
    mount_auth(server).await;

    let inbox = build_inbox(&ctx);
    let names = inbox.pre_warm_channel_names().await;

    assert_eq!(names.len(), 2, "two named channels, the IM is filtered out");
    assert_eq!(
        names.name(&SlackChannelId::new("C1").expect("id")),
        Some("platform-ops")
    );
    assert_eq!(
        names.name(&SlackChannelId::new("C2").expect("id")),
        Some("tech")
    );
    assert_eq!(names.name(&SlackChannelId::new("D9").expect("id")), None);
    assert_eq!(names.workspace(), Some("example"));
}

#[tokio::test]
async fn pre_warm_is_fail_soft_when_list_scope_is_missing() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/conversations.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": false,
            "error": "missing_scope"
        })))
        .mount(server)
        .await;
    mount_auth(server).await;

    let inbox = build_inbox(&ctx);
    let names = inbox.pre_warm_channel_names().await;

    assert!(names.is_empty(), "a missing scope yields an empty name map");
    assert_eq!(
        names.workspace(),
        Some("example"),
        "auth.test still resolves the workspace independently"
    );
}

fn build_inbox(ctx: &common::SlackTestCtx) -> SlackInbox {
    let socket: Arc<dyn SocketModeClient> = MockSocketModeClient::new();
    let factory: SocketFactory = Arc::new(move || Arc::clone(&socket));
    let registry = Arc::new(ListenerRegistry::new());
    let pool = Arc::new(SocketModePool::new(
        slack_client(ctx),
        factory,
        Arc::clone(&registry),
    ));
    let sink = Arc::new(NoopInbox);
    SlackInbox::new(pool, registry, sink as Arc<dyn ChannelInbox>)
}

async fn mount_list(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/conversations.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [
                {"id": "C1", "name": "platform-ops", "is_member": true},
                {"id": "C2", "name": "tech", "is_private": true, "is_member": true},
                {"id": "D9", "is_im": true}
            ],
            "response_metadata": {"next_cursor": ""}
        })))
        .mount(server)
        .await;
}

async fn mount_auth(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/auth.test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "team": "example"
        })))
        .mount(server)
        .await;
}

struct NoopInbox;

#[async_trait::async_trait]
impl ChannelInbox for NoopInbox {
    async fn receive(&self, _event: InboundEvent) -> Result<(), crabgent_channel::ChannelError> {
        Ok(())
    }
}
