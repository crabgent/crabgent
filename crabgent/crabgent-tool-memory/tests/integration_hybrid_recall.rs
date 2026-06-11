use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::{
    EmbeddingError, EmbeddingProvider, EmbeddingRequest, EmbeddingResponse, MemoryId, MemoryScope,
    ModelId, RunCtx, SearchQuery, Subject, Tool, ToolCtx,
};
use crabgent_embedding_fastembed::FastEmbedProvider;
use crabgent_store::{MemoryDoc, MemoryHit, MemoryStore, StoreError};
use crabgent_tool_memory::MemoryTool;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct HybridStore {
    docs: Mutex<Vec<MemoryDoc>>,
}

#[async_trait]
impl MemoryStore for HybridStore {
    async fn search(&self, query: &SearchQuery) -> Result<Vec<MemoryHit>, StoreError> {
        let docs = self.docs.lock().expect("mutex should not be poisoned");
        let mut ranked = docs
            .iter()
            .filter(|doc| query.scope.matches(&doc.scope))
            .map(|doc| {
                let fts_score = lexical_score(&query.query, &doc.body);
                let cosine_similarity = query.embedding.as_ref().and_then(|embedding| {
                    doc.embedding
                        .as_ref()
                        .map(|doc_embedding| cosine(embedding, doc_embedding))
                });
                let rank = fts_score + cosine_similarity.unwrap_or(0.0);
                RankedHit {
                    hit: MemoryHit {
                        id: doc.id.clone(),
                        body: doc.body.clone(),
                        score: fts_score,
                        cosine_similarity,
                        created_at: doc.created_at,
                    },
                    rank,
                }
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| right.rank.total_cmp(&left.rank));
        let offset = usize::try_from(query.offset).unwrap_or(usize::MAX);
        let limit = usize::try_from(query.limit).unwrap_or(usize::MAX);
        Ok(ranked
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|ranked| ranked.hit)
            .collect())
    }

    async fn store(&self, doc: &MemoryDoc) -> Result<MemoryId, StoreError> {
        let mut docs = self.docs.lock().expect("mutex should not be poisoned");
        docs.push(doc.clone());
        Ok(doc.id.clone())
    }

    async fn get(&self, id: &MemoryId) -> Result<Option<MemoryDoc>, StoreError> {
        let docs = self.docs.lock().expect("mutex should not be poisoned");
        Ok(docs
            .iter()
            .find(|doc| doc.id.as_uuid() == id.as_uuid())
            .cloned())
    }

    async fn delete(&self, id: &MemoryId) -> Result<bool, StoreError> {
        let mut docs = self.docs.lock().expect("mutex should not be poisoned");
        let before = docs.len();
        docs.retain(|doc| doc.id.as_uuid() != id.as_uuid());
        Ok(docs.len() != before)
    }

    async fn delete_scoped(&self, id: &MemoryId, scope: &MemoryScope) -> Result<bool, StoreError> {
        let mut docs = self.docs.lock().expect("mutex should not be poisoned");
        let before = docs.len();
        docs.retain(|doc| doc.id.as_uuid() != id.as_uuid() || !scope.matches(&doc.scope));
        Ok(docs.len() != before)
    }

    async fn update_body(&self, id: &MemoryId, new_body: String) -> Result<bool, StoreError> {
        self.update_body_with_embedding(id, new_body, None).await
    }

    async fn update_body_with_embedding(
        &self,
        id: &MemoryId,
        new_body: String,
        embedding: Option<Vec<f32>>,
    ) -> Result<bool, StoreError> {
        let mut docs = self.docs.lock().expect("mutex should not be poisoned");
        let Some(doc) = docs.iter_mut().find(|doc| doc.id.as_uuid() == id.as_uuid()) else {
            return Ok(false);
        };
        doc.body = new_body;
        doc.embedding = embedding;
        Ok(true)
    }
}

struct RankedHit {
    hit: MemoryHit,
    rank: f32,
}

struct KeywordEmbeddingProvider {
    model: ModelId,
}

impl Default for KeywordEmbeddingProvider {
    fn default() -> Self {
        Self {
            model: ModelId::new("test-keyword-embedding"),
        }
    }
}

#[async_trait]
impl EmbeddingProvider for KeywordEmbeddingProvider {
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
        cancel: Option<&CancellationToken>,
    ) -> Result<EmbeddingResponse, EmbeddingError> {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(EmbeddingError::Cancelled);
        }
        Ok(EmbeddingResponse {
            vectors: req.texts.iter().map(|text| embedding_for(text)).collect(),
            model: req.model.unwrap_or_else(|| self.model.clone()),
            dim: self.dim(),
            usage: None,
        })
    }
}

fn lexical_score(query: &str, body: &str) -> f32 {
    let body = body.to_lowercase();
    query
        .split_whitespace()
        .filter(|term| body.contains(&term.to_lowercase()))
        .fold(0.0, |score, _| score + 0.1)
}

fn embedding_for(text: &str) -> Vec<f32> {
    let text = text.to_lowercase();
    if text.contains("noise") {
        vec![1.0, 0.0, 0.0]
    } else if text.contains("payment") || text.contains("invoice") || text.contains("billing") {
        vec![0.0, 1.0, 0.0]
    } else {
        vec![0.0, 0.0, 1.0]
    }
}

fn cosine(left: &[f32], right: &[f32]) -> f32 {
    let dot = left
        .iter()
        .zip(right.iter())
        .map(|(left, right)| left * right)
        .sum::<f32>();
    let left_norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    let denom = left_norm * right_norm;
    if denom <= f32::EPSILON {
        return 0.0;
    }
    dot / denom
}

fn tool(store: &Arc<HybridStore>, embedder: Option<Arc<dyn EmbeddingProvider>>) -> MemoryTool {
    let store_dyn: Arc<dyn MemoryStore> = store.clone();
    MemoryTool::new(store_dyn, Arc::new(AllowAllPolicy), embedder)
}

fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("alice"))
}

async fn store_body(tool: &MemoryTool, body: &str) {
    tool.execute(
        json!({
            "op": "store",
            "scope": {"owner": "alice"},
            "body": body
        }),
        &ctx(),
    )
    .await
    .expect("store should succeed");
}

async fn search_first_body(tool: &MemoryTool) -> String {
    let result = tool
        .execute(
            json!({
                "op": "search",
                "scope": {"owner": "alice"},
                "query": "shared payment"
            }),
            &ctx(),
        )
        .await
        .expect("search should succeed");
    first_body(&result)
}

fn first_body(result: &Value) -> String {
    result["hits"]
        .as_array()
        .expect("hits should be an array")
        .first()
        .expect("search should return at least one hit")["body"]
        .as_str()
        .expect("body should be a string")
        .to_owned()
}

#[test]
fn fastembed_default_dim_matches_bge_m3_plan() {
    assert_eq!(FastEmbedProvider::default_dim(), 1024);
}

#[tokio::test]
async fn hybrid_ranking_differs_from_fts_only() {
    let store = Arc::new(HybridStore::default());
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(KeywordEmbeddingProvider::default());
    let hybrid_tool = tool(&store, Some(embedder));
    let fts_tool = tool(&store, None);

    store_body(&hybrid_tool, "shared payment payment noise").await;
    store_body(&hybrid_tool, "shared invoice billing reference").await;
    store_body(&hybrid_tool, "shared calendar reminder").await;

    let fts_first = search_first_body(&fts_tool).await;
    let hybrid_first = search_first_body(&hybrid_tool).await;

    assert!(fts_first.contains("noise"));
    assert!(hybrid_first.contains("invoice"));
    assert_ne!(fts_first, hybrid_first);
}
