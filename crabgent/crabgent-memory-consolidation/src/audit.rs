//! Audit records for consolidation decisions.

use chrono::{DateTime, Utc};
use crabgent_core::MemoryId;
use crabgent_core::text::truncate_with_ellipsis;
use serde::{Deserialize, Serialize};

use crate::conflict::ConflictDecision;

pub const MAX_AUDIT_REASON_BYTES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsolidationAudit {
    pub decision: ConflictDecision,
    pub reason: String,
    pub source_ids: Vec<MemoryId>,
    pub winner_id: Option<MemoryId>,
    pub created_at: DateTime<Utc>,
}

impl ConsolidationAudit {
    pub fn new(
        decision: ConflictDecision,
        reason: impl Into<String>,
        source_ids: Vec<MemoryId>,
        winner_id: Option<MemoryId>,
        created_at: DateTime<Utc>,
    ) -> Self {
        let reason = reason.into();
        let reason = truncate_with_ellipsis(&reason, MAX_AUDIT_REASON_BYTES, "...").into_owned();
        Self {
            decision,
            reason,
            source_ids,
            winner_id,
            created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_serde_roundtrip() {
        let source_id = MemoryId::new();
        let winner_id = MemoryId::new();
        let audit = ConsolidationAudit::new(
            ConflictDecision::Replace,
            "better semantic wording",
            vec![source_id],
            Some(winner_id),
            Utc::now(),
        );

        let json = serde_json::to_string(&audit).expect("serialize audit");
        let decoded: ConsolidationAudit = serde_json::from_str(&json).expect("deserialize audit");

        assert_eq!(decoded, audit);
    }

    #[test]
    fn audit_reason_truncated_at_500_chars() {
        let audit = ConsolidationAudit::new(
            ConflictDecision::Skip,
            "x".repeat(600),
            Vec::new(),
            None,
            Utc::now(),
        );

        assert_eq!(audit.reason.len(), MAX_AUDIT_REASON_BYTES);
        assert!(audit.reason.ends_with("..."));
    }
}
