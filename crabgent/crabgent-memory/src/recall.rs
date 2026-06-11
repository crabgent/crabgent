//! Recall scoring strategy abstraction.

use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use crabgent_core::SearchQuery;
use crabgent_store::{MemoryDoc, MemoryHit, MemoryStore};

use crate::recall::semantic::SemanticBlend;
use crate::{Clock, MemoryClass, MemoryClassConfig, MemoryError, SystemClock};

pub mod episodic;
pub mod semantic;

#[cfg(test)]
mod test_helpers;

pub trait RecallStrategy: Send + Sync {
    fn score(&self, hit: &MemoryHit, doc_meta: &MemoryDoc, now: DateTime<Utc>) -> f32;
}

#[derive(Clone)]
pub struct MemoryRecall {
    store: Arc<dyn MemoryStore>,
    clock: Arc<dyn Clock>,
}

impl MemoryRecall {
    #[must_use]
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self::with_clock(store, Arc::new(SystemClock))
    }

    #[must_use]
    pub fn with_clock(store: Arc<dyn MemoryStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<MemoryHit>, MemoryError> {
        recall_with_selector(self.store.as_ref(), self.clock.as_ref(), query).await
    }
}

pub async fn recall_with_strategy<S, C>(
    store: &S,
    strategy: &dyn RecallStrategy,
    clock: &C,
    query: &SearchQuery,
) -> Result<Vec<MemoryHit>, MemoryError>
where
    S: MemoryStore + ?Sized,
    C: Clock + ?Sized,
{
    let now = clock.now();
    let hits = store.search(query).await?;
    let mut rescored = Vec::with_capacity(hits.len());

    for mut hit in hits {
        if let Some(doc) = store.get(&hit.id).await? {
            hit.score = strategy.score(&hit, &doc, now);
        } else {
            hit.score = 0.0;
        }
        rescored.push(hit);
    }

    rescored.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    Ok(rescored)
}

async fn recall_with_selector<S, C>(
    store: &S,
    clock: &C,
    query: &SearchQuery,
) -> Result<Vec<MemoryHit>, MemoryError>
where
    S: MemoryStore + ?Sized,
    C: Clock + ?Sized,
{
    let now = clock.now();
    let hits = store.search(query).await?;
    let mut rescored = Vec::with_capacity(hits.len());
    let mut applied_strategy = false;
    // Neutral baseline for hits whose doc has no recognized class. Without it
    // an unclassed hit keeps its raw backend FTS score (e.g. SQLite BM25, on a
    // different scale than the [0, ~1] blend) and can spuriously outrank a
    // rescored hit in a mixed-class result set.
    let neutral = SemanticBlend::default();

    for mut hit in hits {
        let Some(doc) = store.get(&hit.id).await? else {
            hit.score = 0.0;
            rescored.push(hit);
            continue;
        };
        match config_for(query, &doc) {
            Some(config) => {
                hit.score = config.recall.score(&hit, &doc, now);
                applied_strategy = true;
            }
            None => hit.score = neutral.score(&hit, &doc, now),
        }
        rescored.push(hit);
    }

    sort_rescored_hits(&mut rescored, applied_strategy);
    Ok(rescored)
}

fn sort_rescored_hits(rescored: &mut [MemoryHit], applied_strategy: bool) {
    if !applied_strategy {
        return;
    }
    rescored.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
}

fn config_for(query: &SearchQuery, doc: &MemoryDoc) -> Option<MemoryClassConfig> {
    query
        .class
        .as_deref()
        .and_then(parse_class)
        .or_else(|| doc.class.as_deref().and_then(parse_class))
        .map(MemoryClassConfig::defaults_for)
}

fn parse_class(raw: &str) -> Option<MemoryClass> {
    MemoryClass::from_str(raw).ok()
}

pub(crate) fn importance_or_default(doc: &MemoryDoc) -> f32 {
    doc.importance.unwrap_or(0.5).clamp(0.0, 1.0)
}

pub(crate) fn hours_since(created_at: DateTime<Utc>, now: DateTime<Utc>) -> f32 {
    now.signed_duration_since(created_at)
        .to_std()
        .map_or(0.0, |duration| duration.as_secs_f32() / 3_600.0)
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};
    use crabgent_core::MemoryId;
    use crabgent_store::MemoryHit;

    use super::sort_rescored_hits;

    fn hit(body: &str, score: f32, created_at_offset_secs: i64) -> MemoryHit {
        let created_at = Utc
            .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
            .single()
            .expect("valid test datetime")
            + Duration::seconds(created_at_offset_secs);
        MemoryHit {
            id: MemoryId::new(),
            body: body.to_owned(),
            score,
            cosine_similarity: None,
            created_at,
        }
    }

    #[test]
    fn no_strategy_preserves_backend_order() {
        let mut hits = vec![
            hit("backend preferred", 0.1, 0),
            hit("higher score", 0.9, 1),
        ];

        sort_rescored_hits(&mut hits, false);

        assert_eq!(
            hits.iter().map(|hit| hit.body.as_str()).collect::<Vec<_>>(),
            ["backend preferred", "higher score"]
        );
    }

    #[test]
    fn applied_strategy_sorts_by_score_then_created_at() {
        let mut hits = vec![hit("older tie", 1.0, 0), hit("newer tie", 1.0, 1)];

        sort_rescored_hits(&mut hits, true);

        assert_eq!(hits.first().map(|hit| hit.body.as_str()), Some("newer tie"));
    }
}
