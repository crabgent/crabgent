//! Memory importance newtype.

use serde::{Deserialize, Serialize};

use crate::MemoryError;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MemoryImportance(f32);

impl MemoryImportance {
    pub fn new(value: f32) -> Result<Self, MemoryError> {
        if (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(MemoryError::InvalidImportance(value))
        }
    }

    #[must_use]
    pub const fn into_inner(self) -> f32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_range_ok() {
        assert!(
            (MemoryImportance::new(0.5)
                .expect("test result")
                .into_inner()
                - 0.5)
                .abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn negative_errors() {
        assert!(matches!(
            MemoryImportance::new(-0.1),
            Err(MemoryError::InvalidImportance(value)) if value < 0.0
        ));
    }

    #[test]
    fn over_one_errors() {
        assert!(matches!(
            MemoryImportance::new(1.1),
            Err(MemoryError::InvalidImportance(value)) if value > 1.0
        ));
    }

    #[test]
    fn boundary_values_inclusive() {
        assert!(
            (MemoryImportance::new(0.0)
                .expect("test result")
                .into_inner()
                - 0.0)
                .abs()
                < f32::EPSILON
        );
        assert!(
            (MemoryImportance::new(1.0)
                .expect("test result")
                .into_inner()
                - 1.0)
                .abs()
                < f32::EPSILON
        );
    }
}
