mod common;

use crabgent_channel::{Channel, OutboundMessage};
use crabgent_channel_slack::SlackChannel;
use crabgent_core::owner::Owner;
use crabgent_core::subject::Subject;
use serde_json::Value;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Match, Mock, Request, ResponseTemplate};

use common::{slack_client, slack_test_ctx};

#[tokio::test]
async fn top_level_send_omits_thread_ts() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .and(JsonFieldMissing("thread_ts"))
        .and(body_partial_json(serde_json::json!({
            "text": "*top*",
            "mrkdwn": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true, "channel": "C123", "ts": "1.1"
        })))
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let owner = Owner::new("slack:T123/C123");

    let sent = channel
        .send(
            &Subject::new("agent"),
            &owner,
            &OutboundMessage::new("**top**"),
        )
        .await
        .expect("send");

    assert_eq!(sent.thread_root(), None);
    assert_eq!(sent.id, "1.1");
}

struct JsonFieldMissing(&'static str);

impl Match for JsonFieldMissing {
    fn matches(&self, request: &Request) -> bool {
        request
            .body_json::<Value>()
            .ok()
            .is_some_and(|body| body.get(self.0).is_none())
    }
}
