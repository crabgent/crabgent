//! Semantic memory scoring.

use chrono::{DateTime, Utc};
use crabgent_store::{MemoryDoc, MemoryHit};

use crate::recall::{RecallStrategy, importance_or_default};

#[derive(Debug, Default, Clone, Copy)]
pub struct SemanticBlend {
    pub vector_weight: f32,
}

impl SemanticBlend {
    #[must_use]
    pub const fn new(vector_weight: f32) -> Self {
        Self { vector_weight }
    }
}

impl RecallStrategy for SemanticBlend {
    fn score(&self, hit: &MemoryHit, doc_meta: &MemoryDoc, _now: DateTime<Utc>) -> f32 {
        let base = 0.7_f32.mul_add(hit.score, 0.2 * importance_or_default(doc_meta));
        self.vector_weight
            .mul_add(hit.cosine_similarity.unwrap_or(0.0), base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    use crate::recall::test_helpers::{assert_close, doc, hit};

    #[test]
    fn score_uses_fts_weight() {
        let now = Utc
            .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
            .single()
            .expect("valid test datetime");
        let doc = doc("semantic", Some(0.0), now);
        let score = SemanticBlend::default().score(&hit(&doc, 1.0), &doc, now);

        assert_close(score, 0.7);
    }

    #[test]
    fn score_combines_importance() {
        let now = Utc
            .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
            .single()
            .expect("valid test datetime");
        let doc = doc("semantic", Some(1.0), now);
        let score = SemanticBlend::default().score(&hit(&doc, 0.5), &doc, now);

        assert_close(score, 0.55);
    }

    #[test]
    fn score_recency_default_zero() {
        let now = Utc
            .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
            .single()
            .expect("valid test datetime");
        let old = now - Duration::days(7);
        let doc = doc("semantic", Some(0.5), old);
        let recent_score = SemanticBlend::default().score(&hit(&doc, 0.8), &doc, now);
        let later_score =
            SemanticBlend::default().score(&hit(&doc, 0.8), &doc, now + Duration::days(7));

        assert_close(recent_score, later_score);
    }

    #[test]
    fn score_adds_weighted_cosine_similarity() {
        let now = Utc
            .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
            .single()
            .expect("valid test datetime");
        let doc = doc("semantic", Some(1.0), now);
        let mut hit = hit(&doc, 0.5);
        hit.cosine_similarity = Some(1.0);
        let score = SemanticBlend::new(0.5).score(&hit, &doc, now);

        assert_close(score, 1.05);
    }
}
