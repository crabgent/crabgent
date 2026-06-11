//! `OpenAI` Provider implementation for the `crabgent` kernel.

/// Alias keeps `#[crabgent_log::instrument]` proc-macro expansion (which emits
/// `::tracing::*` paths) resolving to `crabgent_log` without re-introducing a
/// direct `tracing` dep.
extern crate crabgent_log as tracing;

pub mod auth;
pub mod client;
pub mod embeddings;
pub mod image_generation;
pub mod models;
pub mod retry;
pub mod stt;
pub mod tts;
pub mod types;
pub mod wire;

use async_trait::async_trait;
use crabgent_core::{
    EventStream, LlmRequest, LlmResponse, ModelInfo, Provider, ProviderCapabilities, ProviderError,
    RunCtx,
};
use crabgent_log::warn;
use secrecy::ExposeSecret;
use serde::Deserialize;
use tokio::time::{Duration, sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::retry::{is_retryable_status, parse_retry_after, sleep_delay};

pub use auth::{ApiKeyAuth, AuthStrategy, CodexOAuthAuth};
pub use embeddings::OpenAiEmbeddingProvider;
pub use image_generation::OpenAiImageGenerationProvider;
pub use stt::{OpenAiSttProvider, SttWsClient, ws::TungsteniteWsClient};
pub use tts::OpenAiTtsProvider;
pub use types::{OpenAiConfig, OpenAiError};
pub use wire::{WireFormat, WireFormatDyn};

const MODEL_DISCOVERY_ENDPOINT: &str = "/v1/models";

/// `OpenAI` API provider.
pub struct OpenAiProvider {
    http: reqwest::Client,
    config: OpenAiConfig,
    auth: Box<dyn AuthStrategy>,
    cached_models: Vec<ModelInfo>,
}

impl OpenAiProvider {
    /// Build a provider with model discovery and fallback to static catalog.
    pub async fn new(
        config: OpenAiConfig,
        auth: Box<dyn AuthStrategy>,
    ) -> Result<Self, OpenAiError> {
        validate_config(&config)?;
        let http = crabgent_provider_transport::hardened_client();

        let cached_models = if auth.supports_model_discovery() {
            match fetch_models_from_api(&http, &config, auth.as_ref()).await {
                Ok(models) => models,
                Err(error) => {
                    warn!(
                        reason = %ProviderError::ModelDiscovery {
                            reason: error.to_string()
                        },
                        "openai model discovery failed, using fallback catalog"
                    );
                    models::openai_models()
                }
            }
        } else {
            models::openai_models()
        };

        Ok(Self {
            http,
            config,
            auth,
            cached_models,
        })
    }

    /// Build a provider from an HTTP client, config, and auth strategy.
    pub fn try_new(
        http: reqwest::Client,
        config: OpenAiConfig,
        auth: Box<dyn AuthStrategy>,
    ) -> Result<Self, OpenAiError> {
        validate_config(&config)?;
        Ok(Self {
            http,
            config,
            auth,
            cached_models: models::openai_models(),
        })
    }

    /// Build a standard API-key provider.
    pub fn try_from_api_key(
        http: reqwest::Client,
        config: OpenAiConfig,
    ) -> Result<Self, OpenAiError> {
        let auth = ApiKeyAuth::new(config.api_key.clone());
        Self::try_new(http, config, Box::new(auth))
    }

    #[must_use]
    pub const fn config(&self) -> &OpenAiConfig {
        &self.config
    }

    #[must_use]
    pub fn auth(&self) -> &dyn AuthStrategy {
        self.auth.as_ref()
    }

    #[must_use]
    pub const fn http_client(&self) -> &reqwest::Client {
        &self.http
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        client::call_complete(
            &self.http,
            &self.config,
            self.auth.as_ref(),
            req,
            ctx,
            cancel,
        )
        .await
    }

    async fn stream(
        &self,
        req: &LlmRequest,
        ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<EventStream, ProviderError> {
        let stream = client::start_stream(
            &self.http,
            &self.config,
            self.auth.as_ref(),
            req,
            ctx,
            cancel,
        )
        .await?;
        Ok(client::into_event_stream(
            stream.response.bytes_stream(),
            stream.decoder,
        ))
    }

    fn name(&self) -> &'static str {
        models::PROVIDER
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.cached_models.clone()
    }

    async fn fetch_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        if !self.auth.supports_model_discovery() {
            return Ok(self.models());
        }
        fetch_models_from_api(&self.http, &self.config, self.auth.as_ref()).await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: true,
            vision: true,
            // Provider-level audio gate. The pre-flight check still
            // requires the routed model's `supports_audio` to be true, so
            // only `gpt-4o-audio-preview` clears it; chat-only models keep
            // `supports_audio: false` and stay blocked.
            audio: true,
            system_prompt: true,
            thinking: true,
            prompt_cache: true,
            max_input_tokens: 400_000,
            max_output_tokens: 128_000,
            web_search: self.auth.supports_hosted_web_search(),
        }
    }
}

#[derive(Deserialize)]
struct ModelDiscoveryResponse {
    #[serde(default)]
    data: Vec<ModelDiscoveryEntry>,
}

#[derive(Deserialize)]
struct ModelDiscoveryEntry {
    id: String,
}

async fn fetch_models_from_api(
    http: &reqwest::Client,
    config: &OpenAiConfig,
    auth: &dyn AuthStrategy,
) -> Result<Vec<ModelInfo>, ProviderError> {
    if !auth.supports_model_discovery() {
        return Ok(models::openai_models());
    }

    let endpoint = model_discovery_url(auth.base_url());
    for attempt in 0..=config.max_retries {
        let response = send_model_discovery_request(http, config, auth, &endpoint).await?;
        if response.status().is_success() {
            return parse_model_discovery_response(response, config).await;
        }

        if response.status().as_u16() == 401 || response.status().as_u16() == 403 {
            return Err(ProviderError::from(OpenAiError::Auth));
        }

        let status = response.status();
        if !is_retryable_status(status.as_u16()) || attempt >= config.max_retries {
            return Err(ProviderError::from(OpenAiError::Api {
                status: status.as_u16(),
                retry_after_secs: parse_retry_after(response.headers())
                    .map(|delay| delay.as_secs()),
            }));
        }

        let retry_after = parse_retry_after(response.headers());
        let delay = sleep_delay(attempt, config.retry_base_delay, retry_after);
        warn!(
            status = status.as_u16(),
            attempt,
            delay_ms = duration_ms(delay),
            "openai transient model-discovery failure, retrying"
        );
        sleep(delay).await;
    }

    Err(ProviderError::Other(
        "openai model discovery exhausted retries".to_owned(),
    ))
}

async fn send_model_discovery_request(
    http: &reqwest::Client,
    config: &OpenAiConfig,
    auth: &dyn AuthStrategy,
    endpoint: &str,
) -> Result<reqwest::Response, ProviderError> {
    let mut request = http.get(endpoint);
    for (name, value) in auth.auth_headers() {
        request = request.header(name, value);
    }

    let request = timeout(config.request_timeout, request.send())
        .await
        .map_err(|_elapsed| ProviderError::Timeout)?;
    request.map_err(|error| {
        if error.is_timeout() {
            ProviderError::Timeout
        } else {
            ProviderError::from(OpenAiError::Network(error.to_string()))
        }
    })
}

async fn parse_model_discovery_response(
    response: reqwest::Response,
    config: &OpenAiConfig,
) -> Result<Vec<ModelInfo>, ProviderError> {
    let bytes = timeout(config.request_timeout, response.bytes())
        .await
        .map_err(|_elapsed| ProviderError::Timeout)?
        .map_err(|error| ProviderError::from(OpenAiError::Network(error.to_string())))?;
    let response: ModelDiscoveryResponse = serde_json::from_slice(&bytes)
        .map_err(|error| ProviderError::from(OpenAiError::MalformedResponse(error.to_string())))?;
    Ok(response
        .data
        .into_iter()
        .filter(|entry| entry.id.starts_with("gpt-"))
        .map(|entry| models::discovered_model(&entry.id))
        .collect())
}

fn model_discovery_url(base_url: &str) -> String {
    format!(
        "{}{}",
        base_url.trim_end_matches('/'),
        MODEL_DISCOVERY_ENDPOINT
    )
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn validate_config(config: &OpenAiConfig) -> Result<(), OpenAiError> {
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
