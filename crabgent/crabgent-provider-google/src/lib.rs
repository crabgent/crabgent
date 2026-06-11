//! Google Gemini provider implementation for the `crabgent` kernel.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

pub mod client;
pub mod image_generation;
pub mod models;
mod request;
mod response;
pub mod types;
mod wire;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::{
    EventStream, LlmRequest, LlmResponse, ModelInfo, Provider, ProviderCapabilities, ProviderError,
    ProviderEvent, RunCtx,
};
use crabgent_log::warn;
use request::{
    build_cached_content_body, build_generate_content_body, build_generate_content_body_with_cache,
};
use response::parse_generate_content_response;
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub use image_generation::GoogleImageGenerationProvider;
pub use types::{GoogleConfig, GoogleError};

/// Google Gemini API provider.
pub struct GoogleProvider {
    http: reqwest::Client,
    config: GoogleConfig,
    cached_models: Vec<ModelInfo>,
    prompt_cache: Arc<Mutex<PromptCache>>,
}

impl GoogleProvider {
    /// Build a provider with model discovery and fallback to the static catalog.
    pub async fn new(config: GoogleConfig) -> Result<Self, GoogleError> {
        let mut provider = Self::try_new(crabgent_provider_transport::hardened_client(), config)?;
        provider.cached_models = match provider.fetch_models().await {
            Ok(models) => models,
            Err(error) => {
                warn!(
                    reason = %error,
                    "google model discovery failed, using fallback catalog"
                );
                models::google_models()
            }
        };
        Ok(provider)
    }

    /// Build a provider from an HTTP client and config.
    pub fn try_new(http: reqwest::Client, config: GoogleConfig) -> Result<Self, GoogleError> {
        validate_config(&config)?;
        Ok(Self {
            http,
            config,
            cached_models: models::google_models(),
            prompt_cache: Arc::new(Mutex::new(PromptCache::default())),
        })
    }

    #[must_use]
    pub const fn config(&self) -> &GoogleConfig {
        &self.config
    }

    #[must_use]
    pub const fn http_client(&self) -> &reqwest::Client {
        &self.http
    }
}

impl GoogleProvider {
    async fn complete_raw(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<(LlmResponse, Option<ProviderEvent>), ProviderError> {
        let (body, cache_creation_tokens) = self.prepare_generate_content_body(req, cancel).await?;
        let value = client::post_json(
            &self.http,
            &self.config,
            req.model.as_str(),
            &body,
            client::MAX_JSON_RESPONSE_BYTES,
            cancel,
        )
        .await?;
        let (mut response, grounding) = parse_generate_content_response(value, req.model.clone())?;
        response.usage.cache_creation_tokens = response
            .usage
            .cache_creation_tokens
            .saturating_add(cache_creation_tokens);
        Ok((response, grounding))
    }

    async fn prepare_generate_content_body(
        &self,
        req: &LlmRequest,
        cancel: Option<&CancellationToken>,
    ) -> Result<(Value, u32), ProviderError> {
        let Some(cache_body) = build_cached_content_body(req) else {
            return Ok((build_generate_content_body(req), 0));
        };
        let cache_key = prompt_cache_key(&cache_body);
        if let Some(body) = self.cached_generate_content_body(req, &cache_key).await {
            return Ok((body, 0));
        }

        Ok(self
            .create_or_fallback_generate_content_body(req, &cache_body, cache_key, cancel)
            .await)
    }

    async fn cached_generate_content_body(
        &self,
        req: &LlmRequest,
        cache_key: &str,
    ) -> Option<Value> {
        let cached_name = self.prompt_cache.lock().await.get(cache_key);
        cached_name.map(|name| build_generate_content_body_with_cache(req, &name))
    }

    async fn create_or_fallback_generate_content_body(
        &self,
        req: &LlmRequest,
        cache_body: &Value,
        cache_key: String,
        cancel: Option<&CancellationToken>,
    ) -> (Value, u32) {
        match client::create_cached_content(&self.http, &self.config, cache_body, cancel).await {
            Ok(value) => self.body_from_created_cache(req, cache_key, value).await,
            Err(error) => {
                warn!(
                    reason = %error,
                    "google prompt cache create failed, using uncached request"
                );
                (build_generate_content_body(req), 0)
            }
        }
    }

    async fn body_from_created_cache(
        &self,
        req: &LlmRequest,
        cache_key: String,
        value: Value,
    ) -> (Value, u32) {
        let Some(created) = CachedContentCreateResult::from_value(value) else {
            warn!("google prompt cache create returned no cache name, using uncached request");
            return (build_generate_content_body(req), 0);
        };
        let name = created.name;
        self.prompt_cache.lock().await.insert(
            cache_key,
            PromptCacheEntry {
                name: name.clone(),
                expires_at: created.expires_at,
                last_used: 0,
            },
        );
        (
            build_generate_content_body_with_cache(req, &name),
            created.creation_tokens,
        )
    }
}

#[async_trait]
impl Provider for GoogleProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let (response, _grounding) = self.complete_raw(req, ctx, cancel).await?;
        Ok(response)
    }

    async fn stream(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<EventStream, ProviderError> {
        let (body, cache_creation_tokens) = self.prepare_generate_content_body(req, cancel).await?;
        let response =
            client::post_stream(&self.http, &self.config, req.model.as_str(), &body, cancel)
                .await?;
        Ok(wire::sse::into_event_stream(
            response.bytes_stream(),
            cache_creation_tokens,
        ))
    }

    fn name(&self) -> &'static str {
        models::PROVIDER
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: true,
            vision: true,
            audio: true,
            system_prompt: true,
            thinking: true,
            prompt_cache: true,
            max_input_tokens: 1_000_000,
            max_output_tokens: 65_536,
            web_search: true,
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.cached_models.clone()
    }

    async fn fetch_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let value = client::get_json(&self.http, &self.config, "/models")
            .await
            .map_err(|error| ProviderError::ModelDiscovery {
                reason: error.to_string(),
            })?;
        let response: ModelsResponse =
            serde_json::from_value(value).map_err(|error| ProviderError::ModelDiscovery {
                reason: error.to_string(),
            })?;
        Ok(response
            .models
            .into_iter()
            .filter_map(|entry| normalize_gemini_model_name(&entry.name))
            .map(|id| models::discovered_model(&id))
            .collect())
    }
}

const PROMPT_CACHE_CAPACITY: usize = 64;

#[derive(Default)]
struct PromptCache {
    entries: HashMap<String, PromptCacheEntry>,
    clock: u64,
}

struct PromptCacheEntry {
    name: String,
    expires_at: Option<DateTime<Utc>>,
    last_used: u64,
}

impl PromptCache {
    fn get(&mut self, key: &str) -> Option<String> {
        self.evict_expired();
        self.clock = self.clock.saturating_add(1);
        let clock = self.clock;
        self.entries.get_mut(key).map(|entry| {
            entry.last_used = clock;
            entry.name.clone()
        })
    }

    fn insert(&mut self, key: String, mut entry: PromptCacheEntry) {
        self.evict_expired();
        self.clock = self.clock.saturating_add(1);
        entry.last_used = self.clock;
        if self.entries.len() >= PROMPT_CACHE_CAPACITY
            && !self.entries.contains_key(&key)
            && let Some(oldest_key) = self.oldest_key()
        {
            self.entries.remove(&oldest_key);
        }
        self.entries.insert(key, entry);
    }

    fn oldest_key(&self) -> Option<String> {
        self.entries
            .iter()
            .min_by_key(|(_key, entry)| entry.last_used)
            .map(|(key, _entry)| key.clone())
    }

    fn evict_expired(&mut self) {
        let now = Utc::now();
        self.entries
            .retain(|_key, entry| entry.expires_at.is_none_or(|expires_at| expires_at > now));
    }
}

#[derive(Deserialize)]
struct CachedContentCreateResponse {
    name: Option<String>,
    #[serde(default, rename = "expireTime")]
    expire_time: Option<String>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<CachedContentUsage>,
}

#[derive(Deserialize)]
struct CachedContentUsage {
    #[serde(default, rename = "totalTokenCount")]
    total: u32,
    #[serde(default, rename = "promptTokenCount")]
    prompt: u32,
}

struct CachedContentCreateResult {
    name: String,
    expires_at: Option<DateTime<Utc>>,
    creation_tokens: u32,
}

impl CachedContentCreateResult {
    fn from_value(value: Value) -> Option<Self> {
        let response: CachedContentCreateResponse = serde_json::from_value(value).ok()?;
        let name = response.name?;
        let expires_at = response.expire_time.as_deref().and_then(parse_rfc3339_utc);
        let creation_tokens = response
            .usage_metadata
            .map(|usage| usage.total.max(usage.prompt))
            .unwrap_or_default();
        Some(Self {
            name,
            expires_at,
            creation_tokens,
        })
    }
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|date_time| date_time.with_timezone(&Utc))
        .ok()
}

fn prompt_cache_key(cache_body: &Value) -> String {
    let bytes = serde_json::to_vec(cache_body).unwrap_or_else(|_| cache_body.to_string().into());
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut out, "{byte:02x}").expect("write to string");
    }
    out
}

fn normalize_gemini_model_name(name: &str) -> Option<String> {
    let id = name.strip_prefix("models/").unwrap_or(name);
    if !id.starts_with("gemini-") {
        return None;
    }
    if id.starts_with("gemini-embedding") || id.starts_with("gemini-robotics") {
        return None;
    }
    if id.contains("-tts")
        || id.contains("computer-use")
        || id.contains("-image")
        || id.contains("native-audio")
        || id.contains("-live")
    {
        return None;
    }
    Some(id.to_owned())
}

fn validate_config(config: &GoogleConfig) -> Result<(), GoogleError> {
    if config.api_key.expose_secret().trim().is_empty() {
        return Err(GoogleError::ConfigError(
            "google api_key must not be empty".to_owned(),
        ));
    }
    if config.base_url.trim().is_empty() {
        return Err(GoogleError::ConfigError(
            "google base_url must not be empty".to_owned(),
        ));
    }
    if config.api_version.trim().is_empty() {
        return Err(GoogleError::ConfigError(
            "google api_version must not be empty".to_owned(),
        ));
    }
    if config.request_timeout.is_zero() {
        return Err(GoogleError::ConfigError(
            "google request_timeout must be > 0".to_owned(),
        ));
    }
    Ok(())
}

impl From<GoogleError> for ProviderError {
    fn from(error: GoogleError) -> Self {
        match error {
            GoogleError::Auth => Self::Auth("google authentication failed".to_owned()),
            GoogleError::Network => Self::Transport("google network error".to_owned()),
            GoogleError::Api {
                status: 429,
                retry_after_secs,
            } => Self::RateLimited { retry_after_secs },
            GoogleError::Api {
                status,
                retry_after_secs,
            } => Self::Api {
                status,
                message: "google api request failed".to_owned(),
                retry_after_secs,
            },
            GoogleError::MalformedResponse(message) => Self::MalformedResponse(message),
            GoogleError::ConfigError(message) => Self::Other(message),
            GoogleError::Cancelled => Self::Cancelled,
            GoogleError::Timeout => Self::Timeout,
        }
    }
}

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    models: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_gemini_model_name_accepts_chat_families() {
        assert_eq!(
            normalize_gemini_model_name("models/gemini-2.5-pro").as_deref(),
            Some("gemini-2.5-pro")
        );
        assert_eq!(
            normalize_gemini_model_name("gemini-3-pro-preview").as_deref(),
            Some("gemini-3-pro-preview")
        );
    }

    #[test]
    fn normalize_gemini_model_name_skips_non_chat_models() {
        for name in [
            "models/gemini-embedding-001",
            "models/gemini-robotics-er-1.5-preview",
            "models/gemini-2.5-pro-preview-tts",
            "models/gemini-3-pro-image-preview",
            "models/gemini-2.0-flash-computer-use-preview",
            "models/gemini-2.5-flash-native-audio-preview-12-2025",
            "models/gemini-live-2.5-flash-preview",
        ] {
            assert_eq!(normalize_gemini_model_name(name), None, "{name}");
        }
    }
}
