//! FastEmbed-backed embedding provider.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::{
    EmbeddingError, EmbeddingProvider, EmbeddingRequest, EmbeddingResponse, ModelId, RunCtx,
};
use fastembed::{EmbeddingModel, ModelTrait, TextEmbedding, TextInitOptions};
use tokio_util::sync::CancellationToken;

const DEFAULT_MODEL_ID: &str = "fastembed/BGEM3";
const DEFAULT_DIM: usize = 1024;

/// FastEmbed-backed local embedding provider.
pub struct FastEmbedProvider {
    model: Arc<Mutex<ModelState>>,
    dim: usize,
    model_id: ModelId,
}

impl FastEmbedProvider {
    /// Build a provider for a `FastEmbed` text embedding model.
    pub fn new(model: EmbeddingModel) -> Result<Self, FastEmbedInitError> {
        Self::new_with_options(model, TextInitOptions::new)
    }

    /// Build a provider with an explicit `FastEmbed` cache directory.
    pub fn new_with_cache_dir(
        model: EmbeddingModel,
        cache_dir: impl Into<PathBuf>,
    ) -> Result<Self, FastEmbedInitError> {
        let cache_dir = cache_dir.into();
        Self::new_with_options(model, |model| {
            TextInitOptions::new(model).with_cache_dir(cache_dir)
        })
    }

    fn new_with_options(
        model: EmbeddingModel,
        options: impl FnOnce(EmbeddingModel) -> TextInitOptions,
    ) -> Result<Self, FastEmbedInitError> {
        let dim = dim_for_model(&model)?;
        let model_id = model_id_for_model(&model);
        let options = options(model).with_show_download_progress(false);
        let model = TextEmbedding::try_new(options)
            .map_err(|error| FastEmbedInitError::new(error.to_string()))?;
        Ok(Self::from_state(
            ModelState::Loaded(Box::new(model)),
            dim,
            model_id,
        ))
    }

    /// Build the default BGE-M3 provider.
    pub fn bge_m3() -> Result<Self, FastEmbedInitError> {
        Self::new(EmbeddingModel::BGEM3)
    }

    #[must_use]
    pub const fn default_dim() -> usize {
        DEFAULT_DIM
    }

    fn from_state(model: ModelState, dim: usize, model_id: ModelId) -> Self {
        Self {
            model: Arc::new(Mutex::new(model)),
            dim,
            model_id,
        }
    }
}

impl fmt::Debug for FastEmbedProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FastEmbedProvider")
            .field("dim", &self.dim)
            .field("model_id", &self.model_id)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl EmbeddingProvider for FastEmbedProvider {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_id(&self) -> &ModelId {
        &self.model_id
    }

    async fn embed(
        &self,
        req: EmbeddingRequest,
        _ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<EmbeddingResponse, EmbeddingError> {
        let texts = req.texts;
        let model_id = req.model.unwrap_or_else(|| self.model_id.clone());
        let model = Arc::clone(&self.model);
        let dim = self.dim;
        let task = tokio::task::spawn_blocking(move || embed_blocking(&model, &texts, dim));

        let vectors = if let Some(token) = cancel {
            tokio::select! {
                biased;
                () = token.cancelled() => return Err(EmbeddingError::Cancelled),
                result = task => join_embedding_task(result)?,
            }
        } else {
            join_embedding_task(task.await)?
        };

        Ok(EmbeddingResponse {
            vectors,
            model: model_id,
            dim,
            usage: None,
        })
    }
}

fn embed_blocking(
    model: &Mutex<ModelState>,
    texts: &[String],
    dim: usize,
) -> Result<Vec<Vec<f32>>, EmbeddingError> {
    let mut guard = model.lock().map_err(|error| {
        EmbeddingError::Other(format!("fastembed model lock poisoned: {error}"))
    })?;
    // Single match keeps the production and test paths in one place. The
    // `Unavailable` arm is only compiled in `cfg(test)`; the production
    // build sees just the `Loaded` arm and clippy then flags the match as
    // infallible, so the lint is expected away only in the non-test cfg
    // where the cfg-removed arm actually disappears.
    #[cfg_attr(
        not(test),
        expect(
            clippy::infallible_destructuring_match,
            reason = "the `Unavailable` arm is cfg(test); under cfg(not(test)) the match collapses to a single arm by design"
        )
    )]
    let model = match &mut *guard {
        ModelState::Loaded(model) => model,
        #[cfg(test)]
        ModelState::Unavailable => {
            return Err(EmbeddingError::Other(
                "fastembed model unavailable".to_owned(),
            ));
        }
    };
    let vectors = model
        .embed(texts, None)
        .map_err(|error| EmbeddingError::Other(format!("fastembed embedding failed: {error}")))?;
    validate_vectors(vectors, texts.len(), dim)
}

fn join_embedding_task(
    result: Result<Result<Vec<Vec<f32>>, EmbeddingError>, tokio::task::JoinError>,
) -> Result<Vec<Vec<f32>>, EmbeddingError> {
    result.map_err(|error| EmbeddingError::Other(format!("fastembed task failed: {error}")))?
}

fn validate_vectors(
    vectors: Vec<Vec<f32>>,
    expected_count: usize,
    dim: usize,
) -> Result<Vec<Vec<f32>>, EmbeddingError> {
    if vectors.len() != expected_count {
        return Err(EmbeddingError::MalformedResponse(format!(
            "fastembed returned {} vectors for {expected_count} inputs",
            vectors.len()
        )));
    }
    if let Some((index, vector)) = vectors
        .iter()
        .enumerate()
        .find(|(_, vector)| vector.len() != dim)
    {
        return Err(EmbeddingError::MalformedResponse(format!(
            "fastembed vector at index {index} has dim {}, expected {dim}",
            vector.len()
        )));
    }
    Ok(vectors)
}

fn dim_for_model(model: &EmbeddingModel) -> Result<usize, FastEmbedInitError> {
    EmbeddingModel::get_model_info(model)
        .map(|info| info.dim)
        .ok_or_else(|| FastEmbedInitError::new(format!("unknown fastembed model: {model}")))
}

fn model_id_for_model(model: &EmbeddingModel) -> ModelId {
    match model {
        EmbeddingModel::BGEM3 => ModelId::new(DEFAULT_MODEL_ID),
        _ => ModelId::new(format!("fastembed/{model}")),
    }
}

enum ModelState {
    Loaded(Box<TextEmbedding>),
    #[cfg(test)]
    Unavailable,
}

/// Error returned while initializing a `FastEmbed` model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FastEmbedInitError {
    message: String,
}

impl FastEmbedInitError {
    const fn new(message: String) -> Self {
        Self { message }
    }
}

impl fmt::Display for FastEmbedInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for FastEmbedInitError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_text_embedding_send<T: Send>() {}

    #[test]
    fn text_embedding_is_send() {
        assert_text_embedding_send::<TextEmbedding>();
    }

    #[test]
    fn bge_m3_dim_is_1024() {
        let provider = FastEmbedProvider::from_state(
            ModelState::Unavailable,
            dim_for_model(&EmbeddingModel::BGEM3).expect("BGEM3 model info exists"),
            model_id_for_model(&EmbeddingModel::BGEM3),
        );

        assert_eq!(provider.dim(), 1024);
        assert_eq!(FastEmbedProvider::default_dim(), 1024);
        assert_eq!(provider.model_id().as_str(), DEFAULT_MODEL_ID);
    }

    #[test]
    fn validate_vectors_rejects_wrong_count() {
        let err = validate_vectors(vec![vec![1.0, 0.0]], 2, 2).expect_err("wrong count rejected");

        assert!(matches!(err, EmbeddingError::MalformedResponse(_)));
    }

    #[test]
    fn validate_vectors_rejects_wrong_dim() {
        let err = validate_vectors(vec![vec![1.0, 0.0]], 1, 3).expect_err("wrong dim rejected");

        assert!(matches!(err, EmbeddingError::MalformedResponse(_)));
    }
}
