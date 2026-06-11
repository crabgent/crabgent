//! Anthropic Provider implementation for the `crabgent` kernel.
//!
//! Talks to the Anthropic Messages API over HTTPS. Supports streaming
//! (SSE) and non-streaming (`/v1/messages`) modes, tool calls, and
//! retry-with-backoff on transient failures (429/500/529 + connect
//! errors). API-key auth via `x-api-key`.
//!
//! The provider keeps auth, fallback, model capability, and request-shaping
//! decisions explicit at the crabgent boundary. Prompt caching is supported
//! through Anthropic request `cache_control` blocks.

pub(crate) mod caching;
pub(crate) mod redact;

pub mod client;
pub mod models;
pub mod request;
pub mod response;
pub mod retry;
pub mod sse_parser;
pub mod types;

use std::collections::VecDeque;
use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use crabgent_core::{
    EventStream, LlmRequest, LlmResponse, ModelInfo, Provider, ProviderCapabilities, ProviderError,
    ProviderEvent, RunCtx,
};
use crabgent_log::warn;
use futures::stream::{self, Stream, StreamExt};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

pub use client::{AnthropicClient, ConfigError};
pub use sse_parser::{ParserLimits, SseError, SseParser};
pub use types::{AnthropicConfig, TtlError};

const ANTHROPIC_TOOL_ADVERTISE_LIMIT: usize = 64;

/// Anthropic Messages API Provider.
pub struct AnthropicProvider {
    client: AnthropicClient,
    parser_limits: ParserLimits,
    cached_models: Vec<ModelInfo>,
}

impl AnthropicProvider {
    /// Build a new provider.
    ///
    /// Attempts runtime model discovery and caches the result. On discovery
    /// failure, logs a warning and falls back to
    /// [`models::anthropic_models`].
    pub async fn new(config: AnthropicConfig) -> Result<Self, ConfigError> {
        let mut provider = Self::try_new(crabgent_provider_transport::hardened_client(), config)?;
        provider.cached_models = match provider.fetch_models().await {
            Ok(models) => models,
            Err(error) => {
                warn!(
                    reason = %error,
                    "anthropic: model discovery failed, using fallback catalog"
                );
                models::anthropic_models()
            }
        };
        Ok(provider)
    }

    /// Build a new provider and return a typed configuration error
    /// instead of panicking.
    pub fn try_new(http: reqwest::Client, config: AnthropicConfig) -> Result<Self, ConfigError> {
        Ok(Self {
            client: AnthropicClient::try_new(http, config)?,
            parser_limits: ParserLimits::default(),
            cached_models: models::anthropic_models(),
        })
    }

    #[must_use]
    pub const fn with_parser_limits(mut self, limits: ParserLimits) -> Self {
        self.parser_limits = limits;
        self
    }

    fn into_event_stream(
        bytes_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
        limits: ParserLimits,
        api_key: &str,
    ) -> EventStream {
        let state = StreamState {
            bytes: Box::pin(bytes_stream),
            parser: SseParser::with_limits(limits).with_api_key(api_key),
            queued: VecDeque::new(),
            finished: false,
        };
        Box::pin(stream::unfold(state, advance_state))
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.client.call_complete(req, cancel).await
    }

    async fn stream(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<EventStream, ProviderError> {
        let response = self.client.start_stream(req, cancel).await?;
        let bytes_stream = response.bytes_stream();
        Ok(Self::into_event_stream(
            bytes_stream,
            self.parser_limits,
            &self.client.config().api_key,
        ))
    }

    fn name(&self) -> &'static str {
        models::PROVIDER
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.cached_models.clone()
    }

    async fn fetch_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let config = self.client.config();
        let response = tokio::time::timeout(
            config.complete_timeout,
            self.client
                .http()
                .get(format!("{}/v1/models", config.endpoint.as_str()))
                .header("x-api-key", config.api_key.as_str())
                .header("anthropic-version", config.anthropic_version.as_str())
                .send(),
        )
        .await
        .map_err(|_elapsed| model_discovery_timeout())?
        .map_err(|error| ProviderError::ModelDiscovery {
            reason: error.to_string(),
        })?;

        let status = response.status();
        if !status.is_success() {
            return Err(ProviderError::ModelDiscovery {
                reason: format!("models endpoint returned {status}"),
            });
        }
        let body = tokio::time::timeout(config.complete_timeout, response.json::<ModelsResponse>())
            .await
            .map_err(|_elapsed| model_discovery_timeout())?
            .map_err(|error| ProviderError::ModelDiscovery {
                reason: error.to_string(),
            })?;

        Ok(populate_model_caps(body.data))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: true,
            vision: true,
            audio: false,
            system_prompt: true,
            thinking: false,
            prompt_cache: true,
            max_input_tokens: 200_000,
            max_output_tokens: 8_192,
            web_search: true,
        }
    }

    fn tool_advertise_limit(&self) -> Option<usize> {
        Some(ANTHROPIC_TOOL_ADVERTISE_LIMIT)
    }
}

fn model_discovery_timeout() -> ProviderError {
    ProviderError::ModelDiscovery {
        reason: "models endpoint timed out".to_owned(),
    }
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<DiscoveredModel>,
}

#[derive(Debug, Deserialize)]
struct DiscoveredModel {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    #[serde(
        rename = "max_input_tokens",
        alias = "input_token_limit",
        alias = "context_window"
    )]
    max_input_tokens: Option<u32>,
    #[serde(default)]
    #[serde(rename = "max_output_tokens", alias = "output_token_limit")]
    max_output_tokens: Option<u32>,
}

fn populate_model_caps(models: Vec<DiscoveredModel>) -> Vec<ModelInfo> {
    let catalog = models::anthropic_models();
    models
        .into_iter()
        .map(|entry| {
            let display_name = entry.display_name.as_deref();
            if let Some(base) = find_hardcoded_match(&entry.id, &catalog) {
                let mut merged = base.clone();
                merged.id = crabgent_core::ModelId::new(entry.id.clone());
                merged.display_name =
                    display_name.map_or_else(|| merged.display_name.clone(), ToOwned::to_owned);
                apply_discovered_limits(&mut merged, &entry);
                merged
            } else {
                let mut fallback = ModelInfo::minimal(entry.id.clone(), models::PROVIDER);
                fallback.display_name =
                    display_name.map_or_else(|| entry.id.clone(), ToOwned::to_owned);
                apply_discovered_limits(&mut fallback, &entry);
                fallback
            }
        })
        .collect()
}

fn apply_discovered_limits(model: &mut ModelInfo, entry: &DiscoveredModel) {
    if let Some(max_input_tokens) = entry.max_input_tokens {
        model.caps.max_input_tokens = max_input_tokens;
    }
    if let Some(max_output_tokens) = entry.max_output_tokens {
        model.caps.max_output_tokens = max_output_tokens;
        model.caps.default_max_output_tokens = model
            .caps
            .max_output_tokens
            .min(model.caps.default_max_output_tokens);
    }
}

fn find_hardcoded_match<'a>(id: &str, catalog: &'a [ModelInfo]) -> Option<&'a ModelInfo> {
    catalog.iter().find(|model| {
        model.id.as_str() == id || model.aliases.iter().any(|alias| alias.as_str() == id)
    })
}

struct StreamState {
    bytes: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    parser: SseParser,
    queued: VecDeque<Result<ProviderEvent, ProviderError>>,
    finished: bool,
}

async fn advance_state(
    mut state: StreamState,
) -> Option<(Result<ProviderEvent, ProviderError>, StreamState)> {
    if let Some(item) = state.queued.pop_front() {
        return Some((item, state));
    }
    while state.queued.is_empty() {
        if state.finished {
            return None;
        }
        match state.bytes.next().await {
            Some(Ok(chunk)) => {
                push_parser_events(&mut state.queued, state.parser.feed(&chunk));
            }
            Some(Err(e)) => {
                state.finished = true;
                state
                    .queued
                    .push_back(Err(ProviderError::Transport(e.to_string())));
            }
            None => {
                state.finished = true;
                let parser = std::mem::take(&mut state.parser);
                push_parser_events(&mut state.queued, parser.finish());
            }
        }
    }
    // invariant: the `while state.queued.is_empty()` loop above only exits
    // by `return None` (finished + still empty) or once the queue holds at
    // least one item, so reaching here guarantees a non-empty queue.
    let next = state.queued.pop_front().expect("queue non-empty");
    Some((next, state))
}

fn push_parser_events(
    queue: &mut VecDeque<Result<ProviderEvent, ProviderError>>,
    events: Vec<Result<ProviderEvent, SseError>>,
) {
    for ev in events {
        queue.push_back(ev.map_err(|err| {
            if err.is_retryable() {
                ProviderError::RetryableStream {
                    message: err.message().to_owned(),
                }
            } else {
                ProviderError::Other(err.message().to_owned())
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_advertise_limit_matches_anthropic_cap() {
        let provider = AnthropicProvider::try_new(
            reqwest::Client::new(),
            AnthropicConfig::new("sk-ant-api03-test"),
        )
        .expect("test config should be valid");

        assert_eq!(provider.tool_advertise_limit(), Some(64));
    }

    #[test]
    fn capabilities_advertise_prompt_cache() {
        let provider = AnthropicProvider::try_new(
            reqwest::Client::new(),
            AnthropicConfig::new("sk-ant-api03-test"),
        )
        .expect("test config should be valid");

        assert!(provider.capabilities().prompt_cache);
    }

    #[test]
    fn capabilities_advertise_web_search() {
        let provider = AnthropicProvider::try_new(
            reqwest::Client::new(),
            AnthropicConfig::new("sk-ant-api03-test"),
        )
        .expect("test config should be valid");

        assert!(provider.capabilities().web_search);
    }
}
