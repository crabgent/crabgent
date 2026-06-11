//! Speech-to-text support for `OpenAI` audio APIs.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{
    SttError, SttEventStream, SttModelInfo, SttProvider, SttProviderCapabilities, SttRequest,
    SttResponse,
};
use secrecy::ExposeSecret;
use tokio_tungstenite::tungstenite::Message;

use crate::auth::{ApiKeyAuth, AuthStrategy};
use crate::types::{OpenAiConfig, OpenAiError};

mod batch;
mod events;
mod models;
pub mod ws;

pub(crate) const REALTIME_ENDPOINT: &str = "/v1/realtime?intent=transcription";

/// WebSocket transport contract for `OpenAI` realtime STT.
#[async_trait]
pub trait SttWsClient: Send + Sync {
    /// Return a client instance isolated to one streaming request.
    fn session_client(&self) -> Arc<dyn SttWsClient>;
    async fn connect(&self, url: &str, headers: Vec<(String, String)>) -> Result<(), SttError>;
    async fn send(&self, message: Message) -> Result<(), SttError>;
    async fn next(&self) -> Result<Option<Message>, SttError>;
    async fn close(&self) -> Result<(), SttError>;
}

/// `OpenAI` speech-to-text provider.
pub struct OpenAiSttProvider {
    config: Arc<OpenAiConfig>,
    auth: Arc<dyn AuthStrategy>,
    http: reqwest::Client,
    ws_client: Arc<dyn SttWsClient>,
}

impl OpenAiSttProvider {
    pub fn try_new(
        http: reqwest::Client,
        config: Arc<OpenAiConfig>,
        auth: Arc<dyn AuthStrategy>,
        ws_client: Arc<dyn SttWsClient>,
    ) -> Result<Self, OpenAiError> {
        validate_config(&config)?;
        Ok(Self {
            config,
            auth,
            http,
            ws_client,
        })
    }

    pub fn try_from_api_key(
        http: reqwest::Client,
        config: OpenAiConfig,
    ) -> Result<Self, OpenAiError> {
        let config = Arc::new(config);
        let auth = Arc::new(ApiKeyAuth::new(config.api_key.clone()));
        Self::try_new(http, config, auth, Arc::new(ws::TungsteniteWsClient::new()))
    }

    #[must_use]
    pub fn config(&self) -> &OpenAiConfig {
        self.config.as_ref()
    }

    #[must_use]
    pub fn auth(&self) -> &dyn AuthStrategy {
        self.auth.as_ref()
    }
}

#[async_trait]
impl SttProvider for OpenAiSttProvider {
    async fn transcribe(&self, req: SttRequest) -> Result<SttResponse, SttError> {
        batch::transcribe_batch(&self.http, self.auth.as_ref(), self.auth.base_url(), req).await
    }

    async fn stream(&self, req: SttRequest) -> Result<SttEventStream, SttError> {
        ws::stream_realtime(Arc::clone(&self.ws_client), self.auth.as_ref(), req).await
    }

    fn capabilities(&self) -> SttProviderCapabilities {
        SttProviderCapabilities {
            streaming: true,
            audio: true,
        }
    }

    fn models(&self) -> Vec<SttModelInfo> {
        models::openai_stt_models()
    }
}

pub(crate) fn validate_config(config: &OpenAiConfig) -> Result<(), OpenAiError> {
    if config.api_key.expose_secret().trim().is_empty() {
        return Err(OpenAiError::ConfigError(
            "openai api_key must not be empty".to_owned(),
        ));
    }
    if config.request_timeout.is_zero() {
        return Err(OpenAiError::ConfigError(
            "openai request_timeout must be > 0".to_owned(),
        ));
    }
    Ok(())
}
