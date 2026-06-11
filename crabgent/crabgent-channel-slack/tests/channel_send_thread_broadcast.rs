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
async fn thread_reply_with_broadcast_sets_reply_broadcast() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .and(JsonFieldIs::new("thread_ts", "1.1"))
        .and(JsonFieldIs::new("reply_broadcast", true))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true, "channel": "C123", "ts": "1.2"
        })))
        .mount(server)
        .await;
    let channel = SlackChannel::new(slack_client(&ctx));
    let owner = Owner::new("slack:T123/C123");
    let parent = MessageRef::thread_reply_broadcast("slack", owner.clone(), "1.0", "1.1", true);
    let msg = OutboundMessage::new("reply").in_thread(parent);

    let sent = channel
        .send(&Subject::new("agent"), &owner, &msg)
        .await
        .expect("send");

    assert_eq!(sent.thread_root(), Some("1.1"));
    assert!(sent.broadcast());
}

struct JsonFieldIs {
    key: &'static str,
    expected: Value,
}

impl JsonFieldIs {
    fn new(key: &'static str, expected: impl Into<Value>) -> Self {
        Self {
            key,
            expected: expected.into(),
        }
    }
}

impl Match for JsonFieldIs {
    fn matches(&self, request: &Request) -> bool {
        request
            .body_json::<Value>()
            .ok()
            .and_then(|body| body.get(self.key).cloned())
            .is_some_and(|value| value == self.expected)
    }
}
