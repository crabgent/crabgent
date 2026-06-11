//! Episodic memory scoring with exponential time decay.

use chrono::{DateTime, Utc};
use crabgent_store::{MemoryDoc, MemoryHit};

use crate::recall::{RecallStrategy, hours_since, importance_or_default};
use crate::{DecayPolicy, MemoryError};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EpisodicBlend {
    policy: DecayPolicy,
    pub vector_weight: f32,
}

impl EpisodicBlend {
    #[must_use]
    pub const fn new(policy: DecayPolicy) -> Self {
        Self {
            policy,
            vector_weight: 0.0,
        }
    }

    #[must_use]
    pub const fn with_vector_weight(mut self, vector_weight: f32) -> Self {
        self.vector_weight = vector_weight;
        self
    }

    pub fn try_new(decay_rate: f32, blend: crate::BlendWeights) -> Result<Self, MemoryError> {
        DecayPolicy::new(decay_rate, blend).map(Self::new)
    }
}

impl Default for EpisodicBlend {
    fn default() -> Self {
        Self::new(DecayPolicy::episodic_default())
    }
}

impl RecallStrategy for EpisodicBlend {
    fn score(&self, hit: &MemoryHit, doc_meta: &MemoryDoc, now: DateTime<Utc>) -> f32 {
        let blend = self.policy.blend;
        let age_hours = hours_since(doc_meta.created_at, now);
        let recency_decayed = (-self.policy.decay_rate * age_hours).exp();

        let base = blend.time_weight.mul_add(
            recency_decayed,
            blend.fts_weight.mul_add(
                hit.score,
                blend.importance_weight * importance_or_default(doc_meta),
            ),
        );
        self.vector_weight
            .mul_add(hit.cosine_similarity.unwrap_or(0.0), base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    use crate::recall::test_helpers::{assert_close, doc, hit};
    use crate::{BlendWeights, Clock, MockClock};

    #[test]
    fn score_decays_with_mock_clock_advance() {
        let start = Utc
            .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
            .single()
            .expect("valid test datetime");
        let clock = MockClock::new(start);
        let blend = BlendWeights::new(0.0, 0.0, 1.0).expect("test result");
        let strategy = EpisodicBlend::try_new(0.05, blend).expect("test result");
        let doc = doc("episodic", Some(0.0), start);
        let hit = hit(&doc, 0.0);

        let initial = strategy.score(&hit, &doc, clock.now());
        clock.advance(chrono::Duration::hours(12));
        let twelve_hours = strategy.score(&hit, &doc, clock.now());
        clock.advance(chrono::Duration::hours(36));
        let forty_eight_hours = strategy.score(&hit, &doc, clock.now());

        assert!(initial > twelve_hours);
        assert!(twelve_hours > forty_eight_hours);
    }

    #[test]
    fn score_blend_weights_apply() {
        let now = Utc
            .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
            .single()
            .expect("valid test datetime");
        let blend = BlendWeights::new(0.25, 0.25, 0.5).expect("test result");
        let strategy = EpisodicBlend::try_new(0.05, blend).expect("test result");
        let doc = doc("episodic", Some(0.8), now);
        let score = strategy.score(&hit(&doc, 0.4), &doc, now);

        assert_close(score, 0.8);
    }

    #[test]
    fn score_adds_weighted_cosine_similarity() {
        let now = Utc
            .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
            .single()
            .expect("valid test datetime");
        let blend = BlendWeights::new(0.25, 0.25, 0.5).expect("test result");
        let strategy = EpisodicBlend::try_new(0.05, blend)
            .expect("test result")
            .with_vector_weight(0.5);
        let doc = doc("episodic", Some(0.8), now);
        let mut hit = hit(&doc, 0.4);
        hit.cosine_similarity = Some(1.0);
        let score = strategy.score(&hit, &doc, now);

        assert_close(score, 1.3);
    }
}
