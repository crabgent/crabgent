//! OpenAI-compatible embedding provider.

use std::time::Duration;

use async_trait::async_trait;
use crabgent_core::{
    EmbeddingError, EmbeddingProvider, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage,
    ModelId, RunCtx,
};
use reqwest::header::CONTENT_TYPE;
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::auth::{ApiKeyAuth, AuthStrategy};
use crate::retry::parse_retry_after;
use crate::types::OpenAiConfig;

const CORTECS_EMBED_BASE_URL: &str = "https://api.cortecs.ai/v1";
const EMBEDDINGS_ENDPOINT: &str = "/embeddings";

/// OpenAI-compatible embedding provider.
pub struct OpenAiEmbeddingProvider {
    http: reqwest::Client,
    config: OpenAiConfig,
    auth: Box<dyn AuthStrategy>,
    model: ModelId,
    dim: usize,
}

impl OpenAiEmbeddingProvider {
    /// Build a Cortecs embedding provider.
    #[must_use]
    pub fn with_cortecs(api_key: SecretString, model: ModelId, dim: usize) -> Self {
        Self::with_openai_compatible_base_url(api_key, CORTECS_EMBED_BASE_URL, model, dim)
    }

    /// Build an embedding provider for an OpenAI-compatible embeddings base URL.
    #[must_use]
    pub fn with_openai_compatible_base_url(
        api_key: SecretString,
        base_url: impl Into<String>,
        model: ModelId,
        dim: usize,
    ) -> Self {
        let config = OpenAiConfig::new(api_key.clone());
        let auth = ApiKeyAuth::new(api_key).with_base_url(base_url.into());
        Self::new(
            crabgent_provider_transport::hardened_client(),
            config,
            Box::new(auth),
            model,
            dim,
        )
    }

    /// Build an embedding provider from explicit HTTP, config, and auth pieces.
    #[must_use]
    pub fn new(
        http: reqwest::Client,
        config: OpenAiConfig,
        auth: Box<dyn AuthStrategy>,
        model: ModelId,
        dim: usize,
    ) -> Self {
        Self {
            http,
            config,
            auth,
            model,
            dim,
        }
    }

    #[must_use]
    pub const fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    #[must_use]
    pub const fn config(&self) -> &OpenAiConfig {
        &self.config
    }

    #[must_use]
    pub fn auth(&self) -> &dyn AuthStrategy {
        self.auth.as_ref()
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbeddingProvider {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_id(&self) -> &ModelId {
        &self.model
    }

    async fn embed(
        &self,
        req: EmbeddingRequest,
        ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<EmbeddingResponse, EmbeddingError> {
        let model = req.model.unwrap_or_else(|| self.model.clone());
        let input_len = req.texts.len();
        let body = json!({
            "model": model.as_str(),
            "input": req.texts,
        });

        let response = send_once(
            &self.http,
            self.config.request_timeout,
            self.auth.as_ref(),
            &body,
            ctx,
            cancel,
        )
        .await?;
        let bytes = read_body(response, self.config.request_timeout, cancel).await?;
        let parsed: ApiEmbeddingResponse = serde_json::from_slice(&bytes)
            .map_err(|error| EmbeddingError::MalformedResponse(error.to_string()))?;
        parse_embedding_response(parsed, input_len, self.dim)
    }
}

async fn send_once(
    http: &reqwest::Client,
    timeout: Duration,
    auth: &dyn AuthStrategy,
    body: &Value,
    ctx: &RunCtx,
    cancel: Option<&CancellationToken>,
) -> Result<reqwest::Response, EmbeddingError> {
    let url = embeddings_url(auth.base_url());
    let mut request = http.post(url).header(CONTENT_TYPE, "application/json");
    for (name, value) in auth.auth_headers() {
        request = request.header(name, value);
    }
    for (name, value) in auth.request_headers(ctx) {
        request = request.header(name, value);
    }
    let request = request.json(body);
    let noop = CancellationToken::new();
    let token = cancel.unwrap_or(&noop);

    let response = tokio::select! {
        biased;
        () = token.cancelled() => return Err(EmbeddingError::Cancelled),
        result = tokio::time::timeout(timeout, request.send()) => result,
    };
    let response = match response {
        Ok(Ok(response)) => response,
        Ok(Err(error)) if error.is_timeout() => return Err(EmbeddingError::Timeout),
        Ok(Err(error)) => return Err(EmbeddingError::Transport(error.to_string())),
        Err(_) => return Err(EmbeddingError::Timeout),
    };

    classify_response(response)
}

fn classify_response(response: reqwest::Response) -> Result<reqwest::Response, EmbeddingError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let retry_after_secs = parse_retry_after(response.headers()).map(|delay| delay.as_secs());
    match status.as_u16() {
        401 | 403 => Err(EmbeddingError::Auth(
            "openai embedding authentication failed".to_owned(),
        )),
        429 => Err(EmbeddingError::RateLimited { retry_after_secs }),
        500..=599 => Err(EmbeddingError::Transport(format!(
            "openai embedding server error: status={}",
            status.as_u16()
        ))),
        status => Err(EmbeddingError::Transport(format!(
            "openai embedding api error: status={status}"
        ))),
    }
}

async fn read_body(
    response: reqwest::Response,
    timeout: Duration,
    cancel: Option<&CancellationToken>,
) -> Result<bytes::Bytes, EmbeddingError> {
    let noop = CancellationToken::new();
    let token = cancel.unwrap_or(&noop);
    tokio::select! {
        biased;
        () = token.cancelled() => Err(EmbeddingError::Cancelled),
        result = tokio::time::timeout(timeout, response.bytes()) => match result {
            Ok(Ok(bytes)) => Ok(bytes),
            Ok(Err(error)) if error.is_timeout() => Err(EmbeddingError::Timeout),
            Ok(Err(error)) => Err(EmbeddingError::Transport(error.to_string())),
            Err(_) => Err(EmbeddingError::Timeout),
        },
    }
}

fn embeddings_url(base_url: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), EMBEDDINGS_ENDPOINT)
}

fn parse_embedding_response(
    response: ApiEmbeddingResponse,
    expected_count: usize,
    dim: usize,
) -> Result<EmbeddingResponse, EmbeddingError> {
    if response.model.trim().is_empty() {
        return Err(EmbeddingError::MalformedResponse(
            "embedding response model is empty".to_owned(),
        ));
    }
    if response.data.len() != expected_count {
        return Err(EmbeddingError::MalformedResponse(format!(
            "embedding response returned {} vectors for {expected_count} inputs",
            response.data.len()
        )));
    }

    let mut slots = vec![None; expected_count];
    for item in response.data {
        let Some(slot) = slots.get_mut(item.index) else {
            return Err(EmbeddingError::MalformedResponse(format!(
                "embedding response index {} out of bounds for {expected_count} inputs",
                item.index
            )));
        };
        if item.embedding.len() != dim {
            return Err(EmbeddingError::MalformedResponse(format!(
                "embedding response vector at index {} has dim {}, expected {dim}",
                item.index,
                item.embedding.len()
            )));
        }
        if slot.replace(item.embedding).is_some() {
            return Err(EmbeddingError::MalformedResponse(format!(
                "embedding response duplicated index {}",
                item.index
            )));
        }
    }

    let mut vectors = Vec::with_capacity(expected_count);
    for (index, slot) in slots.into_iter().enumerate() {
        match slot {
            Some(vector) => vectors.push(vector),
            None => {
                return Err(EmbeddingError::MalformedResponse(format!(
                    "embedding response missing index {index}"
                )));
            }
        }
    }

    Ok(EmbeddingResponse {
        vectors,
        model: ModelId::new(response.model),
        dim,
        usage: response.usage.map(Into::into),
    })
}

#[derive(Deserialize)]
struct ApiEmbeddingResponse {
    data: Vec<ApiEmbeddingData>,
    model: String,
    usage: Option<ApiEmbeddingUsage>,
}

#[derive(Deserialize)]
struct ApiEmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Deserialize)]
struct ApiEmbeddingUsage {
    prompt_tokens: u32,
    total_tokens: u32,
}

impl From<ApiEmbeddingUsage> for EmbeddingUsage {
    fn from(value: ApiEmbeddingUsage) -> Self {
        Self {
            prompt_tokens: value.prompt_tokens,
            total_tokens: value.total_tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crabgent_core::{RunId, Subject};
    use mockito::Matcher;
    use secrecy::SecretString;
    use serde_json::json;

    use super::*;

    const API_KEY_SECRET: &str = "secret-test-key-99999";

    fn config() -> OpenAiConfig {
        OpenAiConfig::new(API_KEY_SECRET)
            .with_max_retries(0)
            .with_request_timeout(Duration::from_secs(2))
    }

    fn provider(base_url: &str) -> OpenAiEmbeddingProvider {
        let auth = ApiKeyAuth::new(SecretString::from(API_KEY_SECRET.to_owned()))
            .with_base_url(base_url.to_owned());
        OpenAiEmbeddingProvider::new(
            reqwest::Client::new(),
            config(),
            Box::new(auth),
            ModelId::new("bge-m3"),
            3,
        )
    }

    fn ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("test"))
    }

    #[tokio::test]
    async fn embedding_success_parses_vectors_by_index() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/embeddings")
            .match_header(
                "authorization",
                Matcher::Exact(format!("Bearer {API_KEY_SECRET}")),
            )
            .match_body(Matcher::PartialJson(json!({
                "model": "bge-m3",
                "input": ["one", "two"],
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "object": "list",
                    "model": "bge-m3",
                    "data": [
                        {"object": "embedding", "index": 1, "embedding": [0.0, 1.0, 0.0]},
                        {"object": "embedding", "index": 0, "embedding": [1.0, 0.0, 0.0]}
                    ],
                    "usage": {"prompt_tokens": 4, "total_tokens": 4}
                })
                .to_string(),
            )
            .create_async()
            .await;

        let response = provider(&format!("{}/v1", server.url()))
            .embed(
                EmbeddingRequest {
                    texts: vec!["one".to_owned(), "two".to_owned()],
                    model: None,
                },
                &ctx(),
                None,
            )
            .await
            .expect("embedding response");

        assert_eq!(
            response.vectors,
            vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]]
        );
        assert_eq!(response.model.as_str(), "bge-m3");
        assert_eq!(
            response.usage,
            Some(EmbeddingUsage {
                prompt_tokens: 4,
                total_tokens: 4,
            })
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn embedding_auth_status_maps_to_opaque_auth_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/embeddings")
            .with_status(401)
            .with_body(format!("bad key {API_KEY_SECRET}"))
            .create_async()
            .await;

        let error = provider(&format!("{}/v1", server.url()))
            .embed(
                EmbeddingRequest {
                    texts: vec!["one".to_owned()],
                    model: None,
                },
                &ctx(),
                None,
            )
            .await
            .expect_err("auth rejected");

        assert!(matches!(error, EmbeddingError::Auth(_)));
        let rendered = format!("{error:?}\n{error}");
        assert!(
            !rendered.contains(API_KEY_SECRET),
            "secret leaked: {rendered}"
        );
        assert!(!rendered.contains("401"), "status leaked: {rendered}");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn embedding_rate_limit_maps_retry_after() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/embeddings")
            .with_status(429)
            .with_header("retry-after", "7")
            .create_async()
            .await;

        let error = provider(&format!("{}/v1", server.url()))
            .embed(
                EmbeddingRequest {
                    texts: vec!["one".to_owned()],
                    model: None,
                },
                &ctx(),
                None,
            )
            .await
            .expect_err("rate limited");

        assert_eq!(
            error,
            EmbeddingError::RateLimited {
                retry_after_secs: Some(7),
            }
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn embedding_server_error_maps_transport() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/embeddings")
            .with_status(500)
            .create_async()
            .await;

        let error = provider(&format!("{}/v1", server.url()))
            .embed(
                EmbeddingRequest {
                    texts: vec!["one".to_owned()],
                    model: None,
                },
                &ctx(),
                None,
            )
            .await
            .expect_err("server error");

        assert!(matches!(error, EmbeddingError::Transport(_)));
        mock.assert_async().await;
    }

    #[test]
    fn embedding_parser_rejects_wrong_dim() {
        let response = ApiEmbeddingResponse {
            model: "bge-m3".to_owned(),
            data: vec![ApiEmbeddingData {
                index: 0,
                embedding: vec![1.0, 0.0],
            }],
            usage: None,
        };

        let error = parse_embedding_response(response, 1, 3).expect_err("wrong dim rejected");

        assert!(matches!(error, EmbeddingError::MalformedResponse(_)));
    }
}
