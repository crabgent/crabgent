//! Recall and lifecycle policy structs.

use serde::{Deserialize, Serialize};

use crate::MemoryError;

const WEIGHT_SUM_EPSILON: f32 = 1e-6;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BlendWeights {
    pub fts_weight: f32,
    pub importance_weight: f32,
    pub time_weight: f32,
}

impl BlendWeights {
    pub fn new(
        fts_weight: f32,
        importance_weight: f32,
        time_weight: f32,
    ) -> Result<Self, MemoryError> {
        let sum = fts_weight + importance_weight + time_weight;
        if sum.is_finite() && (sum - 1.0).abs() <= WEIGHT_SUM_EPSILON {
            Ok(Self {
                fts_weight,
                importance_weight,
                time_weight,
            })
        } else {
            Err(MemoryError::InvalidConfig(format!(
                "blend weights must sum to 1.0, got {sum}"
            )))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DecayPolicy {
    pub decay_rate: f32,
    pub blend: BlendWeights,
}

impl DecayPolicy {
    pub fn new(decay_rate: f32, blend: BlendWeights) -> Result<Self, MemoryError> {
        if decay_rate.is_finite() && decay_rate > 0.0 {
            Ok(Self { decay_rate, blend })
        } else {
            Err(MemoryError::InvalidConfig(format!(
                "decay_rate must be positive, got {decay_rate}"
            )))
        }
    }

    #[must_use]
    pub const fn episodic_default() -> Self {
        Self {
            decay_rate: 0.05,
            blend: BlendWeights {
                fts_weight: 0.5,
                importance_weight: 0.2,
                time_weight: 0.3,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpiryPolicy {
    pub filter_at_search: bool,
    pub filter_archived: bool,
}

impl Default for ExpiryPolicy {
    fn default() -> Self {
        Self {
            filter_at_search: true,
            filter_archived: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_weights_sum_validates() {
        BlendWeights::new(0.5, 0.2, 0.3).expect("test result");
    }

    #[test]
    fn blend_weights_sum_validates_too_low() {
        BlendWeights::new(0.4, 0.2, 0.3).expect_err("expected error");
    }

    #[test]
    fn blend_weights_sum_validates_too_high() {
        BlendWeights::new(0.5, 0.3, 0.3).expect_err("expected error");
    }

    #[test]
    fn epsilon_tolerance_at_boundary() {
        BlendWeights::new(0.5, 0.2, 0.300_000_5).expect("test result");
    }

    #[test]
    fn decay_rate_positive_validates() {
        let blend = BlendWeights::new(0.5, 0.2, 0.3).expect("test result");
        DecayPolicy::new(0.05, blend).expect("test result");
    }

    #[test]
    fn decay_rate_zero_errors() {
        let blend = BlendWeights::new(0.5, 0.2, 0.3).expect("test result");
        DecayPolicy::new(0.0, blend).expect_err("expected error");
    }
}
