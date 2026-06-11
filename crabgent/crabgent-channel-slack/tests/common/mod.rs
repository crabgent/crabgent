#![expect(
    dead_code,
    reason = "shared Slack integration-test helpers are imported by many test binaries with different helper subsets"
)]

use std::sync::Arc;

use crabgent_channel_slack::{SlackConfig, SlackHttpClient};
use crabgent_core::policy::{AllowAllPolicy, DenyAllPolicy, PolicyHook};
use crabgent_core::subject::Subject;
use crabgent_core::tool::ToolCtx;
use secrecy::SecretString;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub struct MockSocketModeClient;

pub struct SlackTestCtx {
    pub app_token: SecretString,
    pub bot_token: SecretString,
    pub test_channel: String,
    pub _mock_server: Option<MockServer>,
    pub socket_mode: Arc<MockSocketModeClient>,
}

impl SlackTestCtx {
    #[expect(
        clippy::used_underscore_binding,
        reason = "_mock_server is the slack-testing.md lifecycle guard field"
    )]
    pub fn mock_server(&self) -> Option<&MockServer> {
        let _socket_refs = Arc::strong_count(&self.socket_mode);
        self._mock_server.as_ref()
    }

    pub fn http_client_with_retry(&self, retry_max: u32) -> SlackHttpClient {
        let mut config = SlackConfig::new(self.app_token.clone(), self.bot_token.clone())
            .expect("test Slack config should be valid")
            .with_retry_max(retry_max);
        if let Some(server) = self.mock_server() {
            config = config.with_api_base(server.uri());
        }
        SlackHttpClient::new(config).expect("test Slack client should build")
    }
}

pub async fn slack_test_ctx() -> SlackTestCtx {
    let app_token = std::env::var("SLACK_APP_TOKEN");
    let bot_token = std::env::var("SLACK_BOT_TOKEN");
    let test_channel = std::env::var("SLACK_TEST_CHANNEL");

    match (app_token, bot_token, test_channel) {
        (Ok(app_token), Ok(bot_token), Ok(test_channel)) => SlackTestCtx {
            app_token: SecretString::from(app_token),
            bot_token: SecretString::from(bot_token),
            test_channel,
            _mock_server: None,
            socket_mode: Arc::new(MockSocketModeClient),
        },
        _ => SlackTestCtx {
            app_token: SecretString::from("app-test-token".to_owned()),
            bot_token: SecretString::from("bot-test-token".to_owned()),
            test_channel: "C123".to_owned(),
            _mock_server: Some(MockServer::start().await),
            socket_mode: Arc::new(MockSocketModeClient),
        },
    }
}

pub fn slack_client(ctx: &SlackTestCtx) -> Arc<SlackHttpClient> {
    Arc::new(ctx.http_client_with_retry(2))
}

pub fn allow_policy() -> Arc<dyn PolicyHook> {
    Arc::new(AllowAllPolicy)
}

pub fn deny_policy() -> Arc<dyn PolicyHook> {
    Arc::new(DenyAllPolicy)
}

pub fn tool_ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("agent"))
}

pub async fn mount_socket_mode_open(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/apps.connections.open"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "url": "wss://mock.socket.test"
        })))
        .mount(server)
        .await;
}
