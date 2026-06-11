//! Hybrid FTS and resolver-based deduplication.

use std::sync::Arc;

use crabgent_core::{
    EmbeddingError, EmbeddingProvider, EmbeddingRequest, MemoryScope, RunCtx, RunId, SearchQuery,
    Subject,
};
use crabgent_memory::{MemoryClass, MemoryRecall};
use crabgent_store::{MemoryDoc, MemoryStore};
use tokio_util::sync::CancellationToken;

use crate::config::{
    CONFLICT_LOWER_DEFAULT, DEFAULT_DEDUP_FTS_CANDIDATES, SIMILARITY_THRESHOLD_DEFAULT,
};
use crate::conflict::{ConflictDecision, ConflictResolver};
use crate::extract::ExtractedFact;
use crate::{ConsolidationError, DedupResult};

pub struct Deduplicator {
    store: Arc<dyn MemoryStore>,
    recall: MemoryRecall,
    similarity_threshold: f64,
    conflict_lower: f64,
    fts_candidates: usize,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
}

impl Deduplicator {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        let recall = MemoryRecall::new(store.clone());
        Self {
            store,
            recall,
            similarity_threshold: SIMILARITY_THRESHOLD_DEFAULT,
            conflict_lower: CONFLICT_LOWER_DEFAULT,
            fts_candidates: DEFAULT_DEDUP_FTS_CANDIDATES,
            embedding_provider: None,
        }
    }

    #[must_use]
    pub const fn with_similarity_threshold(mut self, value: f64) -> Self {
        self.similarity_threshold = value;
        self
    }

    #[must_use]
    pub const fn with_conflict_lower(mut self, value: f64) -> Self {
        self.conflict_lower = value;
        self
    }

    #[must_use]
    pub const fn with_fts_candidates(mut self, value: usize) -> Self {
        self.fts_candidates = value;
        self
    }

    #[must_use]
    pub fn with_embedding_provider(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    pub async fn dedup(
        &self,
        fact: &ExtractedFact,
        scope: &MemoryScope,
        resolver: &dyn ConflictResolver,
        token: &CancellationToken,
    ) -> Result<DedupResult, ConsolidationError> {
        let embedding = self
            .embed_text("memory.consolidation.fact", &fact.content, token)
            .await?;
        let candidate = self.best_candidate(fact, scope, embedding.clone()).await?;
        let Some((existing, similarity)) = candidate else {
            return self.create_fact(fact, scope, embedding).await;
        };

        if similarity >= self.similarity_threshold {
            let updated = self
                .store
                .update_body_with_embedding(&existing.id, fact.content.clone(), embedding)
                .await?;
            return Ok(DedupResult {
                memory_id: Some(existing.id.clone()),
                updated,
                source_ids: vec![existing.id],
                ..DedupResult::default()
            });
        }

        if similarity >= self.conflict_lower {
            return self
                .resolve_conflict(existing, fact, scope, resolver, token, embedding)
                .await;
        }

        self.create_fact(fact, scope, embedding).await
    }

    async fn best_candidate(
        &self,
        fact: &ExtractedFact,
        scope: &MemoryScope,
        embedding: Option<Vec<f32>>,
    ) -> Result<Option<(MemoryDoc, f64)>, ConsolidationError> {
        let limit = u32::try_from(self.fts_candidates).unwrap_or(u32::MAX);
        let class = if fact.kind.trim().is_empty() {
            MemoryClass::Semantic.as_str()
        } else {
            fact.kind.as_str()
        };
        let mut query = SearchQuery::new(&fact.content)
            .scope(scope.clone())
            .class(class)
            .limit(limit);
        if let Some(embedding) = embedding {
            query = query.embedding(embedding);
        }
        let hits = self.recall.search(&query).await?;
        let mut best: Option<(MemoryDoc, f64)> = None;
        for hit in hits {
            let Some(doc) = self.store.get(&hit.id).await? else {
                continue;
            };
            let similarity = text_similarity(&fact.content, &doc.body);
            let replace = match best.as_ref() {
                None => true,
                Some((current, current_similarity)) => {
                    // Deterministic tiebreaker: on equal similarity the
                    // lexicographically smaller id wins, otherwise the
                    // HashMap iter order in the underlying in-memory
                    // store leaks into the consolidation outcome.
                    // `total_cmp` keeps the float compare clippy-safe.
                    match similarity.total_cmp(current_similarity) {
                        std::cmp::Ordering::Greater => true,
                        std::cmp::Ordering::Equal => doc.id < current.id,
                        std::cmp::Ordering::Less => false,
                    }
                }
            };
            if replace {
                best = Some((doc, similarity));
            }
        }
        Ok(best)
    }

    async fn create_fact(
        &self,
        fact: &ExtractedFact,
        scope: &MemoryScope,
        embedding: Option<Vec<f32>>,
    ) -> Result<DedupResult, ConsolidationError> {
        let mut doc = MemoryDoc::new(scope.clone(), fact.content.clone());
        doc.class = Some(fact.kind.clone());
        doc.importance = Some(fact.importance);
        doc.embedding = embedding;
        let id = self.store.store(&doc).await?;
        Ok(DedupResult {
            memory_id: Some(id.clone()),
            created: true,
            source_ids: vec![id],
            ..DedupResult::default()
        })
    }

    async fn resolve_conflict(
        &self,
        existing: MemoryDoc,
        fact: &ExtractedFact,
        scope: &MemoryScope,
        resolver: &dyn ConflictResolver,
        token: &CancellationToken,
        embedding: Option<Vec<f32>>,
    ) -> Result<DedupResult, ConsolidationError> {
        let resolution = resolver.resolve(&existing, fact, token).await?;
        let mut result = DedupResult {
            memory_id: Some(existing.id.clone()),
            conflict: true,
            decision: Some(resolution.decision),
            reason: Some(resolution.reason),
            source_ids: vec![existing.id.clone()],
            ..DedupResult::default()
        };

        match resolution.decision {
            ConflictDecision::Replace => {
                result.updated = self
                    .store
                    .update_body_with_embedding(&existing.id, fact.content.clone(), embedding)
                    .await?;
            }
            ConflictDecision::BothValid => {
                let created = self.create_fact(fact, scope, embedding).await?;
                if let Some(id) = created.memory_id.clone() {
                    result.source_ids.push(id.clone());
                    result.memory_id = Some(id);
                }
                result.created = created.created;
            }
            ConflictDecision::KeepExisting | ConflictDecision::Skip => {}
        }

        Ok(result)
    }

    async fn embed_text(
        &self,
        op: &'static str,
        text: &str,
        token: &CancellationToken,
    ) -> Result<Option<Vec<f32>>, ConsolidationError> {
        let Some(provider) = self.embedding_provider.as_ref() else {
            return Ok(None);
        };
        if text.trim().is_empty() {
            return Ok(None);
        }
        if token.is_cancelled() {
            return Err(ConsolidationError::Cancelled);
        }

        let run_ctx = RunCtx::new(RunId::new(), Subject::new("memory-consolidation"))
            .with_cancel(token.clone());
        let request = EmbeddingRequest {
            texts: vec![text.to_owned()],
            model: None,
        };
        let response = tokio::select! {
            result = provider.embed(request, &run_ctx, Some(token)) => result,
            () = token.cancelled() => return Err(ConsolidationError::Cancelled),
        };
        match response {
            Ok(response) => Ok(extract_single_vector(op, response.vectors, response.dim)),
            Err(EmbeddingError::Cancelled) => Err(ConsolidationError::Cancelled),
            Err(err) => {
                crabgent_log::warn!(
                    op,
                    error_kind = embedding_error_kind(&err),
                    retry_after_secs = embedding_error_retry_after(&err),
                    "memory consolidation embedding failed; storing without embedding"
                );
                Ok(None)
            }
        }
    }
}

fn extract_single_vector(op: &'static str, vectors: Vec<Vec<f32>>, dim: usize) -> Option<Vec<f32>> {
    let mut vectors = vectors.into_iter();
    let vector = first_vector(op, &mut vectors)?;
    if has_extra_vector(op, &mut vectors) {
        return None;
    }
    validate_vector_dim(op, &vector, dim)?;
    Some(vector)
}

fn first_vector(
    op: &'static str,
    vectors: &mut impl Iterator<Item = Vec<f32>>,
) -> Option<Vec<f32>> {
    let vector = vectors.next();
    if vector.is_none() {
        crabgent_log::warn!(
            op,
            "memory consolidation embedding provider returned no vectors"
        );
    }
    vector
}

fn has_extra_vector(op: &'static str, vectors: &mut impl Iterator<Item = Vec<f32>>) -> bool {
    let has_extra = vectors.next().is_some();
    if has_extra {
        crabgent_log::warn!(
            op,
            "memory consolidation embedding provider returned more than one vector"
        );
    }
    has_extra
}

fn validate_vector_dim(op: &'static str, vector: &[f32], dim: usize) -> Option<()> {
    if vector.len() == dim {
        return Some(());
    }
    crabgent_log::warn!(
        op,
        actual_dim = vector.len(),
        expected_dim = dim,
        "memory consolidation embedding provider returned a vector with the wrong dimension"
    );
    None
}

const fn embedding_error_kind(err: &EmbeddingError) -> &'static str {
    match err {
        EmbeddingError::Auth(_) => "auth",
        EmbeddingError::RateLimited { .. } => "rate_limited",
        EmbeddingError::Transport(_) => "transport",
        EmbeddingError::MalformedResponse(_) => "malformed_response",
        EmbeddingError::Cancelled => "cancelled",
        EmbeddingError::Timeout => "timeout",
        EmbeddingError::Other(_) => "other",
        _ => "unknown",
    }
}

const fn embedding_error_retry_after(err: &EmbeddingError) -> Option<u64> {
    match err {
        EmbeddingError::RateLimited { retry_after_secs } => *retry_after_secs,
        _ => None,
    }
}

fn text_similarity(left: &str, right: &str) -> f64 {
    let left_tokens = tokens(left);
    let right_tokens = tokens(right);
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    let overlap = left_tokens
        .iter()
        .filter(|token| right_tokens.contains(token))
        .count();
    let denom = left_tokens.len().max(right_tokens.len());
    usize_to_ratio_component(overlap) / usize_to_ratio_component(denom)
}

fn usize_to_ratio_component(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

fn tokens(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_ascii_lowercase()
        })
        .filter(|token| !token.is_empty())
        .collect()
}
