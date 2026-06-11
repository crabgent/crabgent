//! Per-run auto-disable accounting.
//!
//! Tracks how often each tool's output is recalled within a run. Once a tool
//! crosses the recall threshold the compaction heuristic is clearly wrong for
//! that workload, so the hook stops compacting that tool for the rest of the
//! run. Mirrors the `crabgent-hook-compact` per-run mute pattern
//! (`Arc<tokio::sync::RwLock<..>>`, cleared on `on_stop`).
//!
//! In-memory and per-process: a restart resets the counters.

use std::collections::HashMap;
use std::sync::Arc;

use crabgent_core::run_id::RunId;
use tokio::sync::RwLock;

/// Counts recall expansions per `(run, tool)`.
#[derive(Clone, Default)]
pub struct AutoDisableTracker {
    counts: Arc<RwLock<HashMap<RunId, HashMap<String, u32>>>>,
}

impl AutoDisableTracker {
    /// A fresh, empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one recall/expand of `tool` within `run`.
    pub async fn record(&self, run: &RunId, tool: &str) {
        let mut guard = self.counts.write().await;
        let per_run = guard.entry(run.clone()).or_default();
        *per_run.entry(tool.to_owned()).or_insert(0) += 1;
    }

    /// Whether compaction is disabled for `tool` in `run`.
    ///
    /// A `threshold` of zero is treated as "never auto-disable".
    pub async fn is_disabled(&self, run: &RunId, tool: &str, threshold: u32) -> bool {
        if threshold == 0 {
            return false;
        }
        let guard = self.counts.read().await;
        guard
            .get(run)
            .and_then(|per_run| per_run.get(tool))
            .is_some_and(|count| *count >= threshold)
    }

    /// Drop all counters for a finished run.
    pub async fn clear_run(&self, run: &RunId) {
        self.counts.write().await.remove(run);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn auto_disable_trips_after_n_records() {
        let tracker = AutoDisableTracker::new();
        let run = RunId::new();
        assert!(!tracker.is_disabled(&run, "bash", 3).await);

        tracker.record(&run, "bash").await;
        tracker.record(&run, "bash").await;
        assert!(!tracker.is_disabled(&run, "bash", 3).await);

        tracker.record(&run, "bash").await;
        assert!(tracker.is_disabled(&run, "bash", 3).await);
    }

    #[tokio::test]
    async fn counts_are_per_run_and_per_tool() {
        let tracker = AutoDisableTracker::new();
        let run_a = RunId::new();
        let run_b = RunId::new();
        for _ in 0..3 {
            tracker.record(&run_a, "bash").await;
        }
        assert!(tracker.is_disabled(&run_a, "bash", 3).await);
        // other run untouched.
        assert!(!tracker.is_disabled(&run_b, "bash", 3).await);
        // other tool in the same run untouched.
        assert!(!tracker.is_disabled(&run_a, "read_file", 3).await);
    }

    #[tokio::test]
    async fn clear_run_resets_counters() {
        let tracker = AutoDisableTracker::new();
        let run = RunId::new();
        for _ in 0..5 {
            tracker.record(&run, "bash").await;
        }
        assert!(tracker.is_disabled(&run, "bash", 3).await);
        tracker.clear_run(&run).await;
        assert!(!tracker.is_disabled(&run, "bash", 3).await);
    }

    #[tokio::test]
    async fn threshold_zero_never_disables() {
        let tracker = AutoDisableTracker::new();
        let run = RunId::new();
        tracker.record(&run, "bash").await;
        assert!(!tracker.is_disabled(&run, "bash", 0).await);
    }
}
