mod common;

use crabgent_channel::{Channel, MessageRef, OutboundMessage};
use crabgent_channel_slack::SlackChannel;
use crabgent_core::owner::Owner;
use crabgent_core::subject::Subject;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Match, Mock, Request, ResponseTemplate};

use common::{slack_client, slack_test_ctx};

#[tokio::test]
async fn thread_reply_without_broadcast_omits_reply_broadcast() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .and(JsonFieldMissing("reply_broadcast"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true, "channel": "C123", "ts": "1.2"
        })))
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let owner = Owner::new("slack:T123/C123");
    let parent = MessageRef::thread_reply("slack", owner.clone(), "1.0", "1.1");
    let msg = OutboundMessage::new("reply").in_thread(parent);

    let sent = channel
        .send(&Subject::new("agent"), &owner, &msg)
        .await
        .expect("send");

    assert_eq!(sent.thread_root(), Some("1.1"));
    assert!(!sent.broadcast());
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
