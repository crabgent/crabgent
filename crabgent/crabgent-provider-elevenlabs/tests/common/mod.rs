#![allow(
    dead_code,
    reason = "shared integration-test helpers are used per test target"
)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::SttError;
use crabgent_provider_elevenlabs::{ElevenLabsConfig, SttWsClient};
use tokio_tungstenite::tungstenite::Message;

type RecordedHeaders = Vec<(String, String)>;
type RecordedConnections = Vec<(String, RecordedHeaders)>;

pub struct SttTestCtx {
    pub batch_server: Option<mockito::ServerGuard>,
    pub ws_client: Arc<FakeWsClient>,
    pub config: ElevenLabsConfig,
}

pub async fn stt_test_ctx() -> SttTestCtx {
    let ws_client = Arc::new(FakeWsClient::default());
    if let Ok(key) = std::env::var("ELEVENLABS_TEST_STT_KEY") {
        return SttTestCtx {
            batch_server: None,
            ws_client,
            config: ElevenLabsConfig::new(key),
        };
    }

    let server = mockito::Server::new_async().await;
    SttTestCtx {
        config: ElevenLabsConfig::new("secret-test-xi-key-99999").with_api_base(server.url()),
        batch_server: Some(server),
        ws_client,
    }
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

    pub fn connected_headers(&self) -> Vec<RecordedHeaders> {
        self.shared
            .connected
            .lock()
            .expect("connected lock")
            .iter()
            .map(|(_, headers)| headers.clone())
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
