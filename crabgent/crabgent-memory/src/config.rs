//! Per-class memory configuration presets.

use std::sync::Arc;

use crate::{DecayPolicy, EpisodicBlend, ExpiryPolicy, MemoryClass, RecallStrategy, SemanticBlend};

#[derive(Clone)]
pub struct MemoryClassConfig {
    pub class: MemoryClass,
    pub recall: Arc<dyn RecallStrategy>,
    pub decay: DecayPolicy,
    pub expiry: ExpiryPolicy,
    pub default_importance: f32,
}

impl MemoryClassConfig {
    #[must_use]
    pub fn defaults_for(class: MemoryClass) -> Self {
        match class {
            MemoryClass::Episodic => Self::episodic_defaults(),
            MemoryClass::Semantic
            | MemoryClass::Notes
            | MemoryClass::UserProfile
            | MemoryClass::Skill
            | MemoryClass::Tools => Self::semantic_defaults(),
        }
    }

    pub(crate) fn semantic_defaults() -> Self {
        Self {
            class: MemoryClass::Semantic,
            recall: Arc::new(SemanticBlend::default()),
            decay: DecayPolicy::episodic_default(),
            expiry: ExpiryPolicy::default(),
            default_importance: 0.5,
        }
    }

    pub(crate) fn episodic_defaults() -> Self {
        let decay = DecayPolicy::episodic_default();
        Self {
            class: MemoryClass::Episodic,
            recall: Arc::new(EpisodicBlend::new(decay)),
            decay,
            expiry: ExpiryPolicy::default(),
            default_importance: 0.5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use crabgent_core::{MemoryScope, Owner};
    use crabgent_store::{MemoryDoc, MemoryHit};

    #[derive(Debug, PartialEq)]
    struct ComparableConfig {
        class: MemoryClass,
        decay: DecayPolicy,
        expiry: ExpiryPolicy,
        default_importance: f32,
        recall_score: f32,
    }

    const ALL_VARIANTS: [MemoryClass; 6] = [
        MemoryClass::Semantic,
        MemoryClass::Episodic,
        MemoryClass::Notes,
        MemoryClass::UserProfile,
        MemoryClass::Skill,
        MemoryClass::Tools,
    ];

    fn comparable_config(config: &MemoryClassConfig) -> ComparableConfig {
        let now = Utc
            .with_ymd_and_hms(2026, 5, 12, 8, 0, 0)
            .single()
            .expect("valid test datetime");
        let mut doc = MemoryDoc::new(MemoryScope::for_owner(Owner::new("u")), "body");
        doc.importance = Some(0.8);
        doc.created_at = now;
        doc.updated_at = now;
        let hit = MemoryHit {
            id: doc.id.clone(),
            body: doc.body.clone(),
            score: 0.6,
            cosine_similarity: None,
            created_at: doc.created_at,
        };

        ComparableConfig {
            class: config.class,
            decay: config.decay,
            expiry: config.expiry,
            default_importance: config.default_importance,
            recall_score: config.recall.score(&hit, &doc, now),
        }
    }

    fn assert_defaults_eq(actual: &MemoryClassConfig, expected: &MemoryClassConfig) {
        assert_eq!(comparable_config(actual), comparable_config(expected));
    }

    fn exhaustive_class_match(class: MemoryClass) -> &'static str {
        match class {
            MemoryClass::Semantic => "semantic",
            MemoryClass::Episodic => "episodic",
            MemoryClass::Notes => "notes",
            MemoryClass::UserProfile => "user_profile",
            MemoryClass::Skill => "skill",
            MemoryClass::Tools => "tools",
        }
    }

    #[test]
    fn defaults_for_semantic_uses_semantic_defaults() {
        assert_defaults_eq(
            &MemoryClassConfig::defaults_for(MemoryClass::Semantic),
            &MemoryClassConfig::semantic_defaults(),
        );
    }

    #[test]
    fn defaults_for_episodic_uses_episodic_defaults() {
        assert_defaults_eq(
            &MemoryClassConfig::defaults_for(MemoryClass::Episodic),
            &MemoryClassConfig::episodic_defaults(),
        );
    }

    #[test]
    fn defaults_for_notes_uses_semantic_defaults() {
        assert_defaults_eq(
            &MemoryClassConfig::defaults_for(MemoryClass::Notes),
            &MemoryClassConfig::semantic_defaults(),
        );
    }

    #[test]
    fn defaults_for_user_profile_uses_semantic_defaults() {
        assert_defaults_eq(
            &MemoryClassConfig::defaults_for(MemoryClass::UserProfile),
            &MemoryClassConfig::semantic_defaults(),
        );
    }

    #[test]
    fn defaults_for_skill_uses_semantic_defaults() {
        assert_defaults_eq(
            &MemoryClassConfig::defaults_for(MemoryClass::Skill),
            &MemoryClassConfig::semantic_defaults(),
        );
    }

    #[test]
    fn defaults_for_tools_uses_semantic_defaults() {
        assert_defaults_eq(
            &MemoryClassConfig::defaults_for(MemoryClass::Tools),
            &MemoryClassConfig::semantic_defaults(),
        );
    }

    #[test]
    fn presets_map_to_canonical_classes() {
        assert_eq!(
            MemoryClassConfig::semantic_defaults().class,
            MemoryClass::Semantic
        );
        assert_eq!(
            MemoryClassConfig::episodic_defaults().class,
            MemoryClass::Episodic
        );
    }

    #[test]
    fn exhaustive_class_match_covers_all_variants() {
        for class in ALL_VARIANTS {
            assert_eq!(exhaustive_class_match(class), class.as_str());
        }
    }
}
