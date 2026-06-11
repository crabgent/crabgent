//! `ElevenLabs` STT provider implementation.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{
    SttError, SttEventStream, SttModelInfo, SttProvider, SttProviderCapabilities, SttRequest,
    SttResponse,
};
use secrecy::ExposeSecret;
use serde::Deserialize;

use crate::batch;
use crate::config::ElevenLabsConfig;
use crate::error::ElevenLabsError;
use crate::models::elevenlabs_stt_models;
use crate::ws::{self, SttWsClient};

/// `ElevenLabs` speech-to-text provider.
pub struct ElevenLabsSttProvider {
    config: Arc<ElevenLabsConfig>,
    http: reqwest::Client,
    ws_client: Arc<dyn SttWsClient>,
    cached_models: Vec<SttModelInfo>,
}

impl ElevenLabsSttProvider {
    /// Async constructor. Performs model discovery against `/v1/models` and
    /// falls back to the hardcoded STT model list on error.
    #[crabgent_log::instrument(skip(config))]
    pub async fn new(config: ElevenLabsConfig) -> Result<Self, ElevenLabsError> {
        let mut provider =
            Self::try_from_api_key(crabgent_provider_transport::hardened_client(), config)?;
        match provider.fetch_models().await {
            Ok(models) if !models.is_empty() => {
                provider.cached_models = models;
            }
            Ok(_) => {
                crabgent_log::warn!(
                    "elevenlabs model discovery returned no STT models; using hardcoded catalog"
                );
            }
            Err(error) => {
                crabgent_log::warn!(error = %error, "elevenlabs model discovery failed; using hardcoded catalog");
            }
        }
        Ok(provider)
    }

    pub fn try_new(
        http: reqwest::Client,
        config: Arc<ElevenLabsConfig>,
        ws_client: Arc<dyn SttWsClient>,
    ) -> Result<Self, ElevenLabsError> {
        validate_config(&config)?;
        Ok(Self {
            config,
            http,
            ws_client,
            cached_models: elevenlabs_stt_models(),
        })
    }

    pub fn try_from_api_key(
        http: reqwest::Client,
        config: ElevenLabsConfig,
    ) -> Result<Self, ElevenLabsError> {
        Self::try_new(
            http,
            Arc::new(config),
            Arc::new(ws::TungsteniteWsClient::new()),
        )
    }

    #[must_use]
    pub fn config(&self) -> &ElevenLabsConfig {
        self.config.as_ref()
    }
}

#[async_trait]
impl SttProvider for ElevenLabsSttProvider {
    async fn transcribe(&self, req: SttRequest) -> Result<SttResponse, SttError> {
        batch::transcribe_batch(&self.http, self.config.as_ref(), req).await
    }

    async fn stream(&self, req: SttRequest) -> Result<SttEventStream, SttError> {
        ws::stream_realtime(Arc::clone(&self.ws_client), self.config.as_ref(), req).await
    }

    fn capabilities(&self) -> SttProviderCapabilities {
        SttProviderCapabilities {
            streaming: true,
            audio: true,
        }
    }

    fn models(&self) -> Vec<SttModelInfo> {
        self.cached_models.clone()
    }

    #[crabgent_log::instrument(skip(self))]
    async fn fetch_models(&self) -> Result<Vec<SttModelInfo>, SttError> {
        let response = self
            .http
            .get(models_url(self.config.api_base()))
            .header("xi-api-key", self.config.api_key.expose_secret())
            .send()
            .await
            .map_err(|err| {
                crabgent_log::warn!(error = %err, "elevenlabs model discovery network error");
                discovery_error("elevenlabs model discovery network error")
            })?;

        let status = response.status();
        if !status.is_success() {
            let reason = if matches!(status.as_u16(), 401 | 403) {
                "elevenlabs model discovery authentication failed"
            } else {
                "elevenlabs model discovery request failed"
            };
            return Err(discovery_error(reason));
        }

        let raw = response.json::<Vec<RawModel>>().await.map_err(|err| {
            crabgent_log::warn!(error = %err, "elevenlabs model discovery decode error");
            discovery_error("elevenlabs model discovery decode error")
        })?;

        Ok(raw
            .into_iter()
            // TODO: switch to additive STT capability flag when ElevenLabs API
            // exposes `can_do_speech_to_text` on RawModel.
            .filter(|model| !model.can_do_text_to_speech)
            .map(RawModel::into_model_info)
            .collect())
    }
}

#[derive(Deserialize)]
struct RawModel {
    #[serde(alias = "id")]
    model_id: String,
    #[serde(default)]
    can_do_text_to_speech: bool,
}

impl RawModel {
    fn into_model_info(self) -> SttModelInfo {
        let id = self.model_id;
        elevenlabs_stt_models()
            .into_iter()
            .find(|model| model.id.as_str() == id)
            .unwrap_or_else(|| SttModelInfo {
                id: id.into(),
                supports_streaming: false,
                supports_diarization: false,
            })
    }
}

fn models_url(base_url: &str) -> String {
    format!("{}/v1/models", base_url.trim_end_matches('/'))
}

fn discovery_error(reason: &str) -> SttError {
    SttError::ModelDiscovery {
        reason: reason.to_owned(),
    }
}

fn validate_config(config: &ElevenLabsConfig) -> Result<(), ElevenLabsError> {
    if config.api_key.expose_secret().trim().is_empty() {
        return Err(ElevenLabsError::Config(
            "elevenlabs api_key must not be empty".to_owned(),
        ));
    }
    if config.api_base.trim().is_empty() {
        return Err(ElevenLabsError::Config(
            "elevenlabs api_base must not be empty".to_owned(),
        ));
    }
    Ok(())
}
