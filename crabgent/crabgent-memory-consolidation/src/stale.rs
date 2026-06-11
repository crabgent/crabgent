//! Stale episodic-memory cleanup.

use std::sync::Arc;

use chrono::{Duration, Utc};
use crabgent_core::{MAX_SEARCH_LIMIT, MemoryScope, SearchQuery};
use crabgent_memory::{MemoryClass, MemoryRecall};
use crabgent_store::MemoryStore;

use crate::ConsolidationError;
use crate::config::StalePolicy;

pub struct StaleCleaner {
    store: Arc<dyn MemoryStore>,
    recall: MemoryRecall,
    policy: StalePolicy,
}

impl StaleCleaner {
    pub fn new(store: Arc<dyn MemoryStore>, policy: StalePolicy) -> Self {
        let recall = MemoryRecall::new(store.clone());
        Self {
            store,
            recall,
            policy,
        }
    }

    pub async fn clean(&self, scope: &MemoryScope) -> Result<usize, ConsolidationError> {
        let now = Utc::now();
        let cutoff = now - Duration::days(self.policy.episodic_min_age_days);
        let query = SearchQuery::new("")
            .scope(scope.clone())
            .class(MemoryClass::Episodic.as_str())
            .until(cutoff)
            .limit(MAX_SEARCH_LIMIT);
        let hits = self.recall.search(&query).await?;
        let mut archived = 0;
        for hit in hits {
            let Some(doc) = self.store.get(&hit.id).await? else {
                continue;
            };
            if doc.class.as_deref() != Some(MemoryClass::Episodic.as_str()) {
                continue;
            }
            if doc.importance.unwrap_or(0.5) >= self.policy.importance_threshold {
                continue;
            }
            if self.store.archive(&doc.id, now).await? {
                archived += 1;
            }
        }
        Ok(archived)
    }
}
