//! Embedding provider surface.

use async_trait::async_trait;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::hook::RunCtx;
use crate::model::ModelId;

/// Abstraction over text embedding backends.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embedding vector dimension produced by this provider.
    fn dim(&self) -> usize;

    /// Default model this provider serves when a request does not override it.
    fn model_id(&self) -> &ModelId;

    /// Embed one or more text inputs.
    async fn embed(
        &self,
        req: EmbeddingRequest,
        ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<EmbeddingResponse, EmbeddingError>;
}

/// Request passed to an embedding provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingRequest {
    pub texts: Vec<String>,
    pub model: Option<ModelId>,
}

/// Complete embedding response.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingResponse {
    pub vectors: Vec<Vec<f32>>,
    pub model: ModelId,
    pub dim: usize,
    pub usage: Option<EmbeddingUsage>,
}

/// Token usage reported by an embedding provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingUsage {
    pub prompt_tokens: u32,
    pub total_tokens: u32,
}

/// Errors returned by embedding providers.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum EmbeddingError {
    #[error("auth error: {0}")]
    Auth(String),
    #[error("rate limited: retry after {retry_after_secs:?}s")]
    RateLimited { retry_after_secs: Option<u64> },
    #[error("transport error: {0}")]
    Transport(String),
    #[error("malformed response: {0}")]
    MalformedResponse(String),
    #[error("cancelled")]
    Cancelled,
    #[error("timeout")]
    Timeout,
    #[error("other: {0}")]
    Other(String),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{RunId, Subject};

    struct MockEmbeddingProvider {
        model: ModelId,
    }

    #[async_trait]
    impl EmbeddingProvider for MockEmbeddingProvider {
        fn dim(&self) -> usize {
            3
        }

        fn model_id(&self) -> &ModelId {
            &self.model
        }

        async fn embed(
            &self,
            req: EmbeddingRequest,
            _ctx: &RunCtx,
            _cancel: Option<&CancellationToken>,
        ) -> Result<EmbeddingResponse, EmbeddingError> {
            Ok(EmbeddingResponse {
                vectors: req.texts.iter().map(|_| vec![1.0, 0.0, 0.0]).collect(),
                model: req.model.unwrap_or_else(|| self.model.clone()),
                dim: self.dim(),
                usage: Some(EmbeddingUsage {
                    prompt_tokens: 1,
                    total_tokens: 1,
                }),
            })
        }
    }

    fn assert_object_safe(_provider: Arc<dyn EmbeddingProvider>) {}

    fn test_ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("test-subject"))
    }

    #[test]
    fn embedding_provider_is_object_safe() {
        assert_object_safe(Arc::new(MockEmbeddingProvider {
            model: ModelId::new("embedding-test"),
        }));
    }

    #[tokio::test]
    async fn mock_provider_embeds_all_inputs() {
        let provider = MockEmbeddingProvider {
            model: ModelId::new("embedding-test"),
        };
        let req = EmbeddingRequest {
            texts: vec!["one".to_owned(), "two".to_owned()],
            model: None,
        };

        let response = provider
            .embed(req, &test_ctx(), None)
            .await
            .expect("mock embedding provider returns response");

        assert_eq!(
            response.vectors,
            vec![vec![1.0, 0.0, 0.0], vec![1.0, 0.0, 0.0]]
        );
        assert_eq!(response.model.as_str(), "embedding-test");
        assert_eq!(response.dim, 3);
        assert_eq!(
            response.usage,
            Some(EmbeddingUsage {
                prompt_tokens: 1,
                total_tokens: 1,
            })
        );
    }

    #[test]
    fn embedding_error_display_formats_expected_variants() {
        let auth = EmbeddingError::Auth("bad token".to_owned());
        assert_eq!(auth.to_string(), "auth error: bad token");

        let rate_limited = EmbeddingError::RateLimited {
            retry_after_secs: Some(30),
        };
        assert_eq!(
            rate_limited.to_string(),
            "rate limited: retry after Some(30)s"
        );

        let transport = EmbeddingError::Transport("connection reset".to_owned());
        assert_eq!(transport.to_string(), "transport error: connection reset");

        let malformed = EmbeddingError::MalformedResponse("missing data".to_owned());
        assert_eq!(malformed.to_string(), "malformed response: missing data");

        assert_eq!(EmbeddingError::Cancelled.to_string(), "cancelled");
        assert_eq!(EmbeddingError::Timeout.to_string(), "timeout");
        assert_eq!(
            EmbeddingError::Other("backend unavailable".to_owned()).to_string(),
            "other: backend unavailable"
        );
    }
}
