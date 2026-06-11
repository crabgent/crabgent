mod common;

use crabgent_channel::{Channel, ChannelKind};
use crabgent_channel_slack::SlackChannel;
use crabgent_core::owner::Owner;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use common::{slack_client, slack_test_ctx};

#[tokio::test]
async fn kind_cache_loads_once() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/conversations.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channel": {"id": "C123", "is_im": false}
        })))
        .expect(1)
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let owner = Owner::new("slack:T123/C123");

    assert_eq!(
        channel.kind(&owner).await.expect("first"),
        ChannelKind::Group
    );
    assert_eq!(
        channel.kind(&owner).await.expect("second"),
        ChannelKind::Group
    );
}
