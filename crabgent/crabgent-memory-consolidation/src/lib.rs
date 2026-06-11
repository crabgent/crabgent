//! Memory consolidation primitives.
//!
//! In this crate, a "session" is an episodic `MemoryDoc`, not a
//! `crabgent_session::Session`. `DEFAULT_MIN_EPISODIC_BODY_CHARS` filters short
//! episodic docs before extraction. Consumers can override it with
//! `ConsolidationConfig::with_min_episodic_body_chars`.

pub mod audit;
pub mod checkpoint;
pub mod config;
pub mod conflict;
pub mod cron;
pub mod dedup;
pub mod error;
pub mod extract;
pub mod runner;
pub mod stale;
pub mod types;

pub use audit::{ConsolidationAudit, MAX_AUDIT_REASON_BYTES};
pub use checkpoint::ConsolidationCheckpoint;
pub use config::{
    CHECKPOINT_STALE_AFTER_SECS, CONFLICT_LOWER_DEFAULT, ConsolidationConfig, DEFAULT_CRON_EXPR,
    DEFAULT_CRON_TZ, DEFAULT_DEDUP_FTS_CANDIDATES, DEFAULT_MAX_SESSIONS_PER_RUN,
    DEFAULT_MIN_EPISODIC_BODY_CHARS, SIMILARITY_THRESHOLD_DEFAULT, STALE_EPISODIC_MIN_AGE_DAYS,
    STALE_IMPORTANCE_THRESHOLD, StalePolicy,
};
pub use conflict::{ConflictDecision, ConflictResolution, ConflictResolver, LlmConflictResolver};
pub use cron::{ConsolidationCronExecutor, SubjectResolver};
pub use dedup::Deduplicator;
pub use error::ConsolidationError;
pub use extract::{ExtractedFact, FactExtractor, LlmFactExtractor};
pub use runner::ConsolidationRunner;
pub use stale::StaleCleaner;
pub use types::{
    CLASS_CONSOLIDATION_AUDIT, CLASS_CONSOLIDATION_CHECKPOINT, ConsolidationResult, DedupResult,
};
