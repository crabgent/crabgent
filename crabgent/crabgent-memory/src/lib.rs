//! Memory classification, recall scoring, and optional persistence hooks.

pub mod class;
pub mod clock;
pub mod config;
pub mod error;
pub mod hook;
pub mod importance;
pub mod policy;
pub mod recall;

pub use class::MemoryClass;
pub use clock::{Clock, SystemClock};
pub use config::MemoryClassConfig;
pub use error::MemoryError;
pub use hook::{MemoryPersistHook, PersistClassifier, PersistRequest};
pub use importance::MemoryImportance;
pub use policy::{BlendWeights, DecayPolicy, ExpiryPolicy};
pub use recall::episodic::EpisodicBlend;
pub use recall::semantic::SemanticBlend;
pub use recall::{MemoryRecall, RecallStrategy, recall_with_strategy};

#[cfg(any(test, feature = "test-helpers"))]
pub use clock::MockClock;
