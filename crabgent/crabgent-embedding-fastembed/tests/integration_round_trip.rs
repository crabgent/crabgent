use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crabgent_core::{EmbeddingProvider, EmbeddingRequest, RunCtx, RunId, Subject};
use crabgent_embedding_fastembed::FastEmbedProvider;
use fastembed::EmbeddingModel;

#[tokio::test]
#[ignore = "downloads the FastEmbed model"]
async fn bge_m3_round_trip() -> Result<(), Box<dyn Error>> {
    let cache = TempCache::new()?;
    let provider = FastEmbedProvider::new_with_cache_dir(EmbeddingModel::BGEM3, cache.path())?;
    let response = provider
        .embed(
            EmbeddingRequest {
                texts: vec!["hello from crabgent".to_owned(), "memory search".to_owned()],
                model: None,
            },
            &RunCtx::new(RunId::new(), Subject::new("fastembed-round-trip")),
            None,
        )
        .await?;

    if response.vectors.len() != 2 {
        return Err(std::io::Error::other(format!(
            "expected 2 vectors, got {}",
            response.vectors.len()
        ))
        .into());
    }
    if response.dim != 1024 {
        return Err(
            std::io::Error::other(format!("expected dim 1024, got {}", response.dim)).into(),
        );
    }
    if response.vectors.iter().any(|vector| vector.len() != 1024) {
        return Err(std::io::Error::other("expected every vector to have length 1024").into());
    }
    Ok(())
}

struct TempCache {
    path: PathBuf,
}

impl TempCache {
    fn new() -> std::io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "crabgent-fastembed-cache-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempCache {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.path) {
            Ok(()) | Err(_) => {}
        }
    }
}
