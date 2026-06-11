#![allow(
    dead_code,
    reason = "shared integration-test helpers are used per test target"
)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_core::{LlmRequest, RunCtx, RunId, SttError, Subject, WebSearchConfig};
use crabgent_provider_openai::{
    ApiKeyAuth, AuthStrategy, CodexOAuthAuth, OpenAiConfig, SttWsClient,
};
use secrecy::SecretString;
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

/// Build a baseline Responses `LlmRequest` from the given messages. Shared by
/// the wire-build and tool-choice/web-search wire test targets.
pub fn req(messages: Vec<Value>) -> LlmRequest {
    LlmRequest {
        model: "gpt-5.3-codex".into(),
        system_prompt: None,
        messages,
        tools: Vec::new(),
        max_tokens: Some(128),
        temperature: Some(0.2),
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    }
}

/// A `RunCtx` with a fresh run id and a throwaway subject for wire tests.
pub fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("test"))
}

type RecordedHeaders = Vec<(String, String)>;
type RecordedConnections = Vec<(String, RecordedHeaders)>;

pub struct OpenAiTestCtx {
    pub server: mockito::ServerGuard,
    pub api_key_auth: ApiKeyAuth,
    pub codex_auth: CodexOAuthAuth,
}

pub async fn openai_test_ctx() -> OpenAiTestCtx {
    let server = mockito::Server::new_async().await;
    let base_url = server.url();
    let api_key = SecretString::from("secret-test-key-99999".to_owned());
    let codex_token = SecretString::from("secret-test-token-99999".to_owned());

    OpenAiTestCtx {
        server,
        api_key_auth: ApiKeyAuth::new(api_key).with_base_url(base_url.clone()),
        codex_auth: CodexOAuthAuth::new(codex_token, Some("account-test-id".to_owned()))
            .with_base_url(base_url),
    }
}

pub struct SttTestCtx {
    pub batch_server: Option<mockito::ServerGuard>,
    pub auth: Arc<dyn AuthStrategy>,
    pub ws_client: Arc<FakeWsClient>,
    pub config: OpenAiConfig,
}

pub async fn stt_test_ctx() -> SttTestCtx {
    let ws_client = Arc::new(FakeWsClient::default());
    if let Ok(key) = std::env::var("OPENAI_TEST_STT_KEY") {
        let config = stt_config(key.clone());
        return SttTestCtx {
            batch_server: None,
            auth: Arc::new(ApiKeyAuth::new(SecretString::from(key))),
            ws_client,
            config,
        };
    }

    let server = mockito::Server::new_async().await;
    let api_key = "secret-test-openai-stt-key-99999".to_owned();
    let config = stt_config(api_key.clone());
    let auth = ApiKeyAuth::new(SecretString::from(api_key)).with_base_url(server.url());
    SttTestCtx {
        batch_server: Some(server),
        auth: Arc::new(auth),
        ws_client,
        config,
    }
}

fn stt_config(api_key: String) -> OpenAiConfig {
    OpenAiConfig::new(api_key)
        .with_max_retries(0)
        .with_retry_base_delay(Duration::from_millis(1))
        .with_request_timeout(Duration::from_secs(2))
}

#[derive(Default)]
pub struct FakeWsClient {
    shared: Arc<FakeWsClientState>,
}

#[derive(Default)]
struct FakeWsClientState {
    connected: Mutex<RecordedConnections>,
    sent: Mutex<Vec<Message>>,
    inbound: Mutex<VecDeque<Message>>,
    closed: AtomicBool,
    sessions: AtomicUsize,
}

impl FakeWsClient {
    pub fn push_inbound_message(&self, message: Message) {
        self.shared
            .inbound
            .lock()
            .expect("inbound lock")
            .push_back(message);
    }

    pub fn push_inbound_text(&self, text: impl Into<String>) {
        self.push_inbound_message(Message::Text(text.into().into()));
    }

    pub fn sent_texts(&self) -> Vec<String> {
        self.shared
            .sent
            .lock()
            .expect("sent lock")
            .iter()
            .filter_map(|message| match message {
                Message::Text(text) => Some(text.to_string()),
                _ => None,
            })
            .collect()
    }

    pub fn connected_urls(&self) -> Vec<String> {
        self.shared
            .connected
            .lock()
            .expect("connected lock")
            .iter()
            .map(|(url, _)| url.clone())
            .collect()
    }

    pub fn closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }

    pub fn session_count(&self) -> usize {
        self.shared.sessions.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SttWsClient for FakeWsClient {
    fn session_client(&self) -> Arc<dyn SttWsClient> {
        self.shared.sessions.fetch_add(1, Ordering::SeqCst);
        Arc::new(Self {
            shared: Arc::clone(&self.shared),
        })
    }

    async fn connect(&self, url: &str, headers: Vec<(String, String)>) -> Result<(), SttError> {
        self.shared
            .connected
            .lock()
            .expect("connected lock")
            .push((url.to_owned(), headers));
        Ok(())
    }

    async fn send(&self, message: Message) -> Result<(), SttError> {
        self.shared.sent.lock().expect("sent lock").push(message);
        Ok(())
    }

    async fn next(&self) -> Result<Option<Message>, SttError> {
        Ok(self
            .shared
            .inbound
            .lock()
            .expect("inbound lock")
            .pop_front())
    }

    async fn close(&self) -> Result<(), SttError> {
        self.shared.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}
