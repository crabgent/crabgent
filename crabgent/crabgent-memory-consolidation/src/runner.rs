//! Consolidation pipeline runner.

use std::sync::Arc;

use chrono::Utc;
use crabgent_core::{
    Action, MemoryId, MemoryScope, PolicyDecision, PolicyHook, SearchQuery, Subject,
};
use crabgent_memory::{MemoryClass, MemoryRecall};
use crabgent_store::{MemoryDoc, MemoryRelation, MemoryStore, RelationType, StoreError};
use tokio_util::sync::CancellationToken;

use crate::audit::ConsolidationAudit;
use crate::checkpoint::ConsolidationCheckpoint;
use crate::config::ConsolidationConfig;
use crate::conflict::{ConflictDecision, ConflictResolver};
use crate::dedup::Deduplicator;
use crate::extract::FactExtractor;
use crate::stale::StaleCleaner;
use crate::types::{CLASS_CONSOLIDATION_AUDIT, CLASS_CONSOLIDATION_CHECKPOINT};
use crate::{ConsolidationError, ConsolidationResult, DedupResult};

pub struct ConsolidationRunner {
    store: Arc<dyn MemoryStore>,
    recall: MemoryRecall,
    extractor: Arc<dyn FactExtractor>,
    deduplicator: Deduplicator,
    conflict_resolver: Arc<dyn ConflictResolver>,
    stale_cleaner: StaleCleaner,
    policy: Arc<dyn PolicyHook>,
    config: ConsolidationConfig,
}

impl ConsolidationRunner {
    pub fn new(
        store: Arc<dyn MemoryStore>,
        extractor: Arc<dyn FactExtractor>,
        deduplicator: Deduplicator,
        conflict_resolver: Arc<dyn ConflictResolver>,
        stale_cleaner: StaleCleaner,
        policy: Arc<dyn PolicyHook>,
        config: ConsolidationConfig,
    ) -> Self {
        let recall = MemoryRecall::new(store.clone());
        Self {
            store,
            recall,
            extractor,
            deduplicator,
            conflict_resolver,
            stale_cleaner,
            policy,
            config,
        }
    }

    pub async fn run(
        &self,
        subject: &Subject,
        scope: MemoryScope,
        token: CancellationToken,
    ) -> Result<ConsolidationResult, ConsolidationError> {
        self.policy_gate(subject, &scope).await?;
        let mut lease = self.acquire_checkpoint(&scope).await?;
        let mut result = ConsolidationResult::default();

        let pipeline = self
            .run_with_checkpoint(&scope, &token, &lease.checkpoint, &mut result)
            .await;
        let release = self
            .release_checkpoint(&mut lease, pipeline.is_ok().then_some(&result))
            .await;
        match (pipeline, release) {
            (Err(err), _) | (Ok(()), Err(err)) => Err(err),
            (Ok(()), Ok(())) => Ok(result),
        }
    }

    async fn run_with_checkpoint(
        &self,
        scope: &MemoryScope,
        token: &CancellationToken,
        checkpoint: &ConsolidationCheckpoint,
        result: &mut ConsolidationResult,
    ) -> Result<(), ConsolidationError> {
        result.stale_archived = self.stale_cleaner.clean(scope).await?;
        self.run_pipeline(scope, token, checkpoint, result).await
    }

    pub async fn status(
        &self,
        subject: &Subject,
        scope: MemoryScope,
    ) -> Result<Option<ConsolidationCheckpoint>, ConsolidationError> {
        self.policy_gate(subject, &scope).await?;
        Ok(self
            .find_checkpoint(&scope)
            .await?
            .map(|(_, _, checkpoint)| checkpoint))
    }

    async fn policy_gate(
        &self,
        subject: &Subject,
        scope: &MemoryScope,
    ) -> Result<(), ConsolidationError> {
        let action = Action::MemoryConsolidate {
            scope: scope.clone(),
        };
        match self.policy.allow(subject, &action).await {
            PolicyDecision::Allow => Ok(()),
            PolicyDecision::Deny(reason) => Err(ConsolidationError::Denied(reason)),
        }
    }

    async fn run_pipeline(
        &self,
        scope: &MemoryScope,
        token: &CancellationToken,
        checkpoint: &ConsolidationCheckpoint,
        result: &mut ConsolidationResult,
    ) -> Result<(), ConsolidationError> {
        let docs = self.episodic_docs(scope, checkpoint.last_run_at).await?;
        for doc in docs {
            if token.is_cancelled() {
                return Err(ConsolidationError::Cancelled);
            }
            if doc.body.chars().count() < self.config.min_episodic_body_chars {
                continue;
            }
            let facts = self.extractor.extract(&doc, token).await?;
            result.sessions_processed += 1;
            result.facts_extracted += facts.len();
            for fact in facts {
                let dedup = self
                    .deduplicator
                    .dedup(&fact, scope, self.conflict_resolver.as_ref(), token)
                    .await?;
                self.apply_dedup_result(scope, &dedup, result).await?;
                self.emit_relations(scope, &doc.id, &dedup).await;
            }
            result.last_processed(doc.id);
        }
        Ok(())
    }

    async fn episodic_docs(
        &self,
        scope: &MemoryScope,
        since: Option<chrono::DateTime<Utc>>,
    ) -> Result<Vec<MemoryDoc>, ConsolidationError> {
        let limit = u32::try_from(self.config.max_sessions_per_run).unwrap_or(u32::MAX);
        let mut query = SearchQuery::new("")
            .scope(scope.clone())
            .class(MemoryClass::Episodic.as_str())
            .limit(limit);
        if let Some(since) = since {
            query = query.since(since);
        }
        let hits = self.recall.search(&query).await?;
        let mut docs = Vec::with_capacity(hits.len());
        for hit in hits {
            if let Some(doc) = self.store.get(&hit.id).await? {
                docs.push(doc);
            }
        }
        Ok(docs)
    }

    async fn apply_dedup_result(
        &self,
        scope: &MemoryScope,
        dedup: &DedupResult,
        result: &mut ConsolidationResult,
    ) -> Result<(), ConsolidationError> {
        if dedup.created {
            result.memories_created += 1;
        }
        if dedup.updated {
            result.memories_updated += 1;
        }
        if dedup.conflict {
            result.conflicts_detected += 1;
        }
        // Kept separate from the counter bump above: this block is fallible
        // (audit write can fail with `?`) and performs a store side effect,
        // so merging would entangle pure counting with I/O.
        if dedup.conflict {
            self.write_audit(scope, dedup).await?;
            result.audits_written += 1;
        }
        Ok(())
    }

    async fn write_audit(
        &self,
        scope: &MemoryScope,
        dedup: &DedupResult,
    ) -> Result<(), ConsolidationError> {
        let audit = ConsolidationAudit::new(
            dedup.decision.unwrap_or(crate::ConflictDecision::Skip),
            dedup.reason.clone().unwrap_or_default(),
            dedup.source_ids.clone(),
            dedup.memory_id.clone(),
            Utc::now(),
        );
        let body = serde_json::to_string(&audit).map_err(StoreError::Serialization)?;
        let mut doc = MemoryDoc::new(scope.clone(), body);
        doc.class = Some(CLASS_CONSOLIDATION_AUDIT.to_owned());
        self.store.store(&doc).await?;
        Ok(())
    }

    /// Persist the relation edge implied by a dedup outcome.
    ///
    /// Unlike `write_audit`, which propagates store failures with `?`, relation
    /// emission is intentionally fail-open: a missing edge degrades the graph
    /// layer but must not abort an otherwise successful consolidation run, so a
    /// store error is logged and swallowed here.
    async fn emit_relations(&self, scope: &MemoryScope, source_id: &MemoryId, dedup: &DedupResult) {
        let Some((from, to, relation_type)) = classify_relation(source_id, dedup) else {
            return;
        };
        let relation = MemoryRelation::new(from, to, relation_type, scope.clone());
        if let Err(err) = self.store.relation_store(&relation).await {
            crabgent_log::warn!(
                op = "relation_store",
                error_kind = err.kind(),
                "consolidation relation emission failed; skipping edge"
            );
        }
    }

    async fn acquire_checkpoint(
        &self,
        scope: &MemoryScope,
    ) -> Result<CheckpointLease, ConsolidationError> {
        let existing = self.find_checkpoint(scope).await?;
        let now = Utc::now();
        let Some((id, doc, checkpoint)) = existing else {
            let checkpoint = ConsolidationCheckpoint {
                in_progress: true,
                ..ConsolidationCheckpoint::default()
            };
            let id = self.store_checkpoint(scope, &checkpoint).await?;
            return Ok(CheckpointLease { id, checkpoint });
        };

        if checkpoint.in_progress && !checkpoint.is_stale(doc.updated_at, now) {
            return Err(ConsolidationError::AlreadyRunning(scope.clone()));
        }

        let mut active = checkpoint.clone();
        active.in_progress = true;
        self.update_checkpoint(&id, &active).await?;
        // The lease intentionally carries the pre-mutation snapshot, not
        // `active`. The run reads only `checkpoint.last_run_at` (unchanged by
        // the `in_progress` flip), and `release_checkpoint` accumulates onto
        // the snapshot's `sessions_processed` before overwriting `in_progress`
        // to false. Storing `active` here would be observationally identical
        // for accumulation but would hand callers a flag that is about to be
        // reset anyway.
        Ok(CheckpointLease { id, checkpoint })
    }

    async fn release_checkpoint(
        &self,
        lease: &mut CheckpointLease,
        result: Option<&ConsolidationResult>,
    ) -> Result<(), ConsolidationError> {
        lease.checkpoint.in_progress = false;
        if let Some(result) = result {
            lease.checkpoint.last_run_at = Some(Utc::now());
            lease.checkpoint.sessions_processed += result.sessions_processed;
            if let Some(last_processed_id) = result.last_processed_id.clone() {
                lease.checkpoint.last_processed_id = Some(last_processed_id);
            }
        }
        self.update_checkpoint(&lease.id, &lease.checkpoint).await
    }

    async fn find_checkpoint(
        &self,
        scope: &MemoryScope,
    ) -> Result<Option<(MemoryId, MemoryDoc, ConsolidationCheckpoint)>, ConsolidationError> {
        let query = SearchQuery::new("")
            .scope(scope.clone())
            .class(CLASS_CONSOLIDATION_CHECKPOINT)
            .include_archived()
            .limit(1);
        // Checkpoints use an internal marker class, not a user-facing memory
        // class, so this lookup intentionally bypasses recall scoring.
        let Some(hit) = self.store.search(&query).await?.into_iter().next() else {
            return Ok(None);
        };
        let Some(doc) = self.store.get(&hit.id).await? else {
            return Ok(None);
        };
        let checkpoint = serde_json::from_str(&doc.body).map_err(StoreError::Serialization)?;
        Ok(Some((doc.id.clone(), doc, checkpoint)))
    }

    async fn store_checkpoint(
        &self,
        scope: &MemoryScope,
        checkpoint: &ConsolidationCheckpoint,
    ) -> Result<MemoryId, ConsolidationError> {
        let body = serde_json::to_string(checkpoint).map_err(StoreError::Serialization)?;
        let mut doc = MemoryDoc::new(scope.clone(), body);
        doc.class = Some(CLASS_CONSOLIDATION_CHECKPOINT.to_owned());
        Ok(self.store.store(&doc).await?)
    }

    async fn update_checkpoint(
        &self,
        id: &MemoryId,
        checkpoint: &ConsolidationCheckpoint,
    ) -> Result<(), ConsolidationError> {
        let body = serde_json::to_string(checkpoint).map_err(StoreError::Serialization)?;
        self.store.update_body(id, body).await?;
        Ok(())
    }
}

struct CheckpointLease {
    id: MemoryId,
    checkpoint: ConsolidationCheckpoint,
}

trait ResultExt {
    fn last_processed(&mut self, id: MemoryId);
}

impl ResultExt for ConsolidationResult {
    fn last_processed(&mut self, id: MemoryId) {
        self.last_processed_id = Some(id);
    }
}

/// Map a dedup outcome to the relation edge it implies, returning
/// `(from_id, to_id, relation_type)`. Pure: no I/O, so the per-fact loop and
/// `emit_relations` stay below the cognitive-complexity cap.
fn classify_relation(
    source_id: &MemoryId,
    dedup: &DedupResult,
) -> Option<(MemoryId, MemoryId, RelationType)> {
    let fact_id = dedup.memory_id.clone()?;
    if dedup.conflict {
        return match dedup.decision {
            // The episodic source supersedes the fact it replaced. Guarded on
            // `updated`: a stale dedup hit whose `update_body` returned false
            // never wrote the replacement, so no supersedes edge is implied.
            Some(ConflictDecision::Replace) if dedup.updated => {
                Some((source_id.clone(), fact_id, RelationType::supersedes()))
            }
            // On BothValid the dedup stored a fresh fact (`memory_id`) and kept
            // the existing one as `source_ids[0]`: the new fact contradicts it.
            Some(ConflictDecision::BothValid) => dedup
                .source_ids
                .first()
                .map(|existing| (fact_id, existing.clone(), RelationType::contradicts())),
            _ => None,
        };
    }
    if dedup.created {
        // A brand-new fact derives from its episodic source.
        return Some((fact_id, source_id.clone(), RelationType::derived_from()));
    }
    if dedup.updated {
        // A high-similarity restatement: the source supports the existing fact.
        return Some((source_id.clone(), fact_id, RelationType::supports()));
    }
    None
}
