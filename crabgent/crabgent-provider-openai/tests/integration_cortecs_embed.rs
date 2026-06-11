use std::env;
use std::error::Error;

use crabgent_core::{EmbeddingProvider, EmbeddingRequest, ModelId, RunCtx, RunId, Subject};
use crabgent_provider_openai::OpenAiEmbeddingProvider;
use secrecy::SecretString;

const CORTECS_BASE_URL: &str = "https://api.cortecs.ai/v1";
const BGE_M3_MODEL: &str = "bge-m3";
const BGE_M3_DIM: usize = 1024;

#[tokio::test]
async fn cortecs_bge_m3_embedding_smoke() -> Result<(), Box<dyn Error>> {
    drop(dotenvy::dotenv());
    let Some((api_key, base_url)) = live_embedding_config() else {
        return Ok(());
    };
    let provider = OpenAiEmbeddingProvider::with_openai_compatible_base_url(
        SecretString::from(api_key),
        base_url,
        ModelId::new(BGE_M3_MODEL),
        BGE_M3_DIM,
    );
    let response = provider
        .embed(
            EmbeddingRequest {
                texts: vec!["hello from crabgent".to_owned()],
                model: None,
            },
            &RunCtx::new(RunId::new(), Subject::new("integration-cortecs-embed")),
            None,
        )
        .await?;

    if response.dim != BGE_M3_DIM {
        return Err(std::io::Error::other(format!(
            "expected dim {BGE_M3_DIM}, got {}",
            response.dim
        ))
        .into());
    }
    if response.model.as_str() != BGE_M3_MODEL {
        return Err(std::io::Error::other(format!(
            "expected model {BGE_M3_MODEL}, got {}",
            response.model.as_str()
        ))
        .into());
    }
    if response.vectors.len() != 1 {
        return Err(std::io::Error::other(format!(
            "expected one vector, got {}",
            response.vectors.len()
        ))
        .into());
    }
    let Some(vector) = response.vectors.first() else {
        return Err(std::io::Error::other("embedding response contains no vector").into());
    };
    if vector.len() != BGE_M3_DIM {
        return Err(std::io::Error::other(format!(
            "expected vector dim {BGE_M3_DIM}, got {}",
            vector.len()
        ))
        .into());
    }
    Ok(())
}

fn live_embedding_config() -> Option<(String, String)> {
    if let Ok(api_key) = env::var("CORTECS_API_KEY") {
        return Some((api_key, CORTECS_BASE_URL.to_owned()));
    }

    let api_key = env::var("OPENAI_API_KEY").ok()?;
    let base_url = env::var("OPENAI_EMBED_BASE_URL").ok()?;
    Some((api_key, base_url))
}
