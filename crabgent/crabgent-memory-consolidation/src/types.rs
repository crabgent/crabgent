//! Shared data types for memory consolidation.

use crabgent_core::MemoryId;

use crate::conflict::ConflictDecision;

pub const CLASS_CONSOLIDATION_AUDIT: &str = "consolidation_audit";
pub const CLASS_CONSOLIDATION_CHECKPOINT: &str = "consolidation_checkpoint";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsolidationResult {
    pub sessions_processed: usize,
    pub facts_extracted: usize,
    pub memories_created: usize,
    pub memories_updated: usize,
    pub conflicts_detected: usize,
    pub audits_written: usize,
    pub stale_archived: usize,
    pub last_processed_id: Option<MemoryId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DedupResult {
    pub memory_id: Option<MemoryId>,
    pub created: bool,
    pub updated: bool,
    pub conflict: bool,
    pub decision: Option<ConflictDecision>,
    pub reason: Option<String>,
    pub source_ids: Vec<MemoryId>,
}
