//! In-memory backend for [`MemoryStore`].
//!
//! Naive substring matcher. The `SQLite` backend uses FTS5 BM25; this
//! one returns score `1.0` for every hit and orders by importance
//! descending, then `created_at` descending, then `MemoryId`
//! ascending as the final deterministic tiebreaker so the underlying
//! `HashMap` iteration order cannot leak into search results when the
//! first two keys tie.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::{MemoryId, MemoryScope, SearchQuery};

use crate::error::StoreError;
use crate::ids::RelationId;
use crate::memory_search::MemorySearchPlan;
use crate::records::{MemoryDoc, MemoryHit, MemoryRelation};
use crate::relation_type::RelationType;
use crate::scope_query::ScopeQuery;
use crate::traits::MemoryStore;

#[derive(Default)]
pub struct MemoryMemoryStore {
    inner: Mutex<HashMap<MemoryId, MemoryDoc>>,
    // Edges live in a Vec, not a HashMap keyed by RelationId: every relation
    // operation (idempotency lookup, neighbor scan, natural-key delete,
    // cascade) is a linear scan, so a Vec is the simplest fit.
    relations: Mutex<Vec<MemoryRelation>>,
}

impl MemoryMemoryStore {
    fn lock(&self) -> Result<MutexGuard<'_, HashMap<MemoryId, MemoryDoc>>, StoreError> {
        self.inner
            .lock()
            .map_err(|e| StoreError::backend(format!("memory mutex poisoned: {e}")))
    }

    fn lock_relations(&self) -> Result<MutexGuard<'_, Vec<MemoryRelation>>, StoreError> {
        self.relations
            .lock()
            .map_err(|e| StoreError::backend(format!("relation mutex poisoned: {e}")))
    }

    /// True when both linked documents exist and are visible under the edge
    /// `scope`. Visibility uses the same owner-with-shared widening as
    /// [`MemoryStore::relation_neighbors`]: an owner-scoped caller only sees its
    /// own and the agent's shared docs, never another owner's. A wildcard field
    /// (e.g. a global edge scope with no owner) does not constrain that field,
    /// so consolidation runs over a broad scope keep working.
    fn both_docs_visible(
        &self,
        from_id: &MemoryId,
        to_id: &MemoryId,
        scope: &MemoryScope,
    ) -> Result<bool, StoreError> {
        let docs = self.lock()?;
        let mut visibility = ScopeQuery::filter(scope);
        if let Some(agent) = scope.agent.as_deref() {
            visibility.widen_owner_to_shared(agent);
        }
        let visible = |id: &MemoryId| {
            docs.get(id)
                .is_some_and(|doc| visibility.matches(&doc.scope))
        };
        Ok(visible(from_id) && visible(to_id))
    }

    /// Remove every edge incident to `id` (as `from_id` or `to_id`). Called on
    /// document delete so the graph never keeps edges over removed nodes.
    fn cascade_relations(&self, id: &MemoryId) -> Result<(), StoreError> {
        let mut relations = self.lock_relations()?;
        relations.retain(|edge| &edge.from_id != id && &edge.to_id != id);
        Ok(())
    }
}

/// Edges share a natural key: same endpoints, same label, same owner.
fn natural_key_eq(left: &MemoryRelation, right: &MemoryRelation) -> bool {
    left.from_id == right.from_id
        && left.to_id == right.to_id
        && left.relation_type == right.relation_type
        && left.scope.owner == right.scope.owner
}

#[async_trait]
impl MemoryStore for MemoryMemoryStore {
    async fn search(&self, query: &SearchQuery) -> Result<Vec<MemoryHit>, StoreError> {
        let inner = self.lock()?;
        let plan = MemorySearchPlan::new(query);
        let q_lower = plan.text.map(str::to_lowercase);
        let mut docs: Vec<_> = inner
            .values()
            .filter(|doc| plan.matches_doc(doc))
            .filter(|doc| {
                q_lower
                    .as_ref()
                    .is_none_or(|text| doc.body.to_lowercase().contains(text))
            })
            .collect();
        docs.sort_by(|left, right| {
            right
                .importance
                .unwrap_or(0.5)
                .total_cmp(&left.importance.unwrap_or(0.5))
                .then_with(|| right.created_at.cmp(&left.created_at))
                // Final id-tiebreaker keeps ordering deterministic when
                // multiple docs land in the same microsecond and share
                // importance; without it the underlying HashMap iter
                // order leaks through the stable sort.
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(docs
            .into_iter()
            .skip(plan.offset_usize)
            .take(plan.limit_usize)
            // In-memory backend: vector search is not supported, so search
            // returns FTS-only-equivalent hits.
            .map(|doc| MemoryHit {
                id: doc.id.clone(),
                body: doc.body.clone(),
                score: 1.0,
                cosine_similarity: None,
                created_at: doc.created_at,
            })
            .collect())
    }

    async fn store(&self, doc: &MemoryDoc) -> Result<MemoryId, StoreError> {
        let id = doc.id.clone();
        let mut inner = self.lock()?;
        inner.insert(id.clone(), doc.clone());
        Ok(id)
    }

    async fn get(&self, id: &MemoryId) -> Result<Option<MemoryDoc>, StoreError> {
        let inner = self.lock()?;
        Ok(inner.get(id).cloned())
    }

    async fn delete(&self, id: &MemoryId) -> Result<bool, StoreError> {
        let removed = {
            let mut inner = self.lock()?;
            inner.remove(id).is_some()
        };
        if removed {
            self.cascade_relations(id)?;
        }
        Ok(removed)
    }

    async fn delete_scoped(&self, id: &MemoryId, scope: &MemoryScope) -> Result<bool, StoreError> {
        let removed = {
            let mut inner = self.lock()?;
            let Some(doc) = inner.get(id) else {
                return Ok(false);
            };
            if !ScopeQuery::filter(scope).matches(&doc.scope) {
                return Ok(false);
            }
            inner.remove(id).is_some()
        };
        if removed {
            self.cascade_relations(id)?;
        }
        Ok(removed)
    }

    async fn archive(&self, id: &MemoryId, at: DateTime<Utc>) -> Result<bool, StoreError> {
        let mut inner = self.lock()?;
        let Some(doc) = inner.get_mut(id) else {
            return Ok(false);
        };
        doc.archived_at = Some(at);
        doc.updated_at = at;
        Ok(true)
    }

    async fn unarchive(&self, id: &MemoryId) -> Result<bool, StoreError> {
        let mut inner = self.lock()?;
        let Some(doc) = inner.get_mut(id) else {
            return Ok(false);
        };
        doc.archived_at = None;
        doc.updated_at = Utc::now();
        Ok(true)
    }

    async fn extend_expiry(
        &self,
        id: &MemoryId,
        new_expiry: Option<DateTime<Utc>>,
    ) -> Result<bool, StoreError> {
        let mut inner = self.lock()?;
        let Some(doc) = inner.get_mut(id) else {
            return Ok(false);
        };
        doc.expires_at = new_expiry;
        doc.updated_at = Utc::now();
        Ok(true)
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
        let mut inner = self.lock()?;
        let Some(doc) = inner.get_mut(id) else {
            return Ok(false);
        };
        doc.body = new_body;
        doc.embedding = embedding;
        doc.updated_at = Utc::now();
        Ok(true)
    }

    async fn relation_store(&self, relation: &MemoryRelation) -> Result<RelationId, StoreError> {
        // Lock docs first (and drop) before relations to keep a single global
        // lock order; the two guards are never held at once. Both endpoints must
        // be visible to the edge scope, so a caller cannot link (or probe the
        // existence of) another owner's documents.
        if !self.both_docs_visible(&relation.from_id, &relation.to_id, &relation.scope)? {
            return Err(StoreError::NotFound);
        }
        let mut relations = self.lock_relations()?;
        if let Some(existing) = relations.iter().find(|edge| natural_key_eq(edge, relation)) {
            return Ok(existing.id.clone());
        }
        let id = relation.id.clone();
        relations.push(relation.clone());
        Ok(id)
    }

    async fn relation_delete(
        &self,
        from_id: &MemoryId,
        to_id: &MemoryId,
        relation_type: &RelationType,
        scope: &MemoryScope,
    ) -> Result<bool, StoreError> {
        let mut relations = self.lock_relations()?;
        let before = relations.len();
        relations.retain(|edge| {
            !(&edge.from_id == from_id
                && &edge.to_id == to_id
                && &edge.relation_type == relation_type
                && edge.scope.owner == scope.owner)
        });
        Ok(relations.len() != before)
    }

    async fn relation_neighbors(
        &self,
        ids: &[MemoryId],
        scope: &MemoryScope,
    ) -> Result<Vec<MemoryRelation>, StoreError> {
        let mut visibility = ScopeQuery::filter(scope);
        if let Some(agent) = scope.agent.as_deref() {
            visibility.widen_owner_to_shared(agent);
        }
        let relations = self.lock_relations()?;
        Ok(relations
            .iter()
            .filter(|edge| ids.contains(&edge.from_id) || ids.contains(&edge.to_id))
            .filter(|edge| visibility.matches(&edge.scope))
            .cloned()
            .collect())
    }
}

#[cfg(test)]
#[path = "memory_store_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "memory_store_relation_tests.rs"]
mod relation_tests;
