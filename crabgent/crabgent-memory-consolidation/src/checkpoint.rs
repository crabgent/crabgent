//! Checkpoint state for consolidation runs.

use chrono::{DateTime, Duration, Utc};
use crabgent_core::MemoryId;
use serde::{Deserialize, Serialize};

use crate::config::CHECKPOINT_STALE_AFTER_SECS;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsolidationCheckpoint {
    #[serde(default)]
    pub in_progress: bool,
    #[serde(default)]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_processed_id: Option<MemoryId>,
    #[serde(default)]
    pub sessions_processed: usize,
}

impl ConsolidationCheckpoint {
    pub fn is_stale(&self, updated_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        self.is_stale_after(
            updated_at,
            now,
            Duration::seconds(CHECKPOINT_STALE_AFTER_SECS),
        )
    }

    pub fn is_stale_after(
        &self,
        updated_at: DateTime<Utc>,
        now: DateTime<Utc>,
        stale_after: Duration,
    ) -> bool {
        self.in_progress && now.signed_duration_since(updated_at) > stale_after
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_serde_roundtrip() {
        let checkpoint = ConsolidationCheckpoint {
            in_progress: true,
            last_run_at: Some(Utc::now()),
            last_processed_id: Some(MemoryId::new()),
            sessions_processed: 7,
        };

        let json = serde_json::to_string(&checkpoint).expect("serialize checkpoint");
        let decoded: ConsolidationCheckpoint =
            serde_json::from_str(&json).expect("deserialize checkpoint");

        assert_eq!(decoded, checkpoint);
    }

    #[test]
    fn checkpoint_in_progress_default_false() {
        let checkpoint = ConsolidationCheckpoint::default();

        assert!(!checkpoint.in_progress);
    }

    #[test]
    fn checkpoint_stale_after_one_hour() {
        let now = Utc::now();
        let checkpoint = ConsolidationCheckpoint {
            in_progress: true,
            ..ConsolidationCheckpoint::default()
        };

        assert!(checkpoint.is_stale(now - Duration::seconds(3601), now));
        assert!(!checkpoint.is_stale(now - Duration::seconds(3599), now));
    }
}
