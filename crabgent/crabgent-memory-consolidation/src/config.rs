//! Consolidation configuration defaults.

pub const DEFAULT_CRON_EXPR: &str = "0 4 * * *";
pub const DEFAULT_CRON_TZ: &str = "Europe/Berlin";
pub const STALE_EPISODIC_MIN_AGE_DAYS: i64 = 30;
pub const STALE_IMPORTANCE_THRESHOLD: f32 = 0.3;
pub const DEFAULT_DEDUP_FTS_CANDIDATES: usize = 10;
pub const DEFAULT_MAX_SESSIONS_PER_RUN: usize = 50;
pub const DEFAULT_MIN_EPISODIC_BODY_CHARS: usize = 80;
pub const CHECKPOINT_STALE_AFTER_SECS: i64 = 3600;
pub const CONFLICT_LOWER_DEFAULT: f64 = 0.7;
pub const SIMILARITY_THRESHOLD_DEFAULT: f64 = 0.85;

#[derive(Debug, Clone, PartialEq)]
pub struct ConsolidationConfig {
    pub cron_expr: String,
    pub cron_tz: String,
    pub stale_policy: StalePolicy,
    pub dedup_fts_candidates: usize,
    pub max_sessions_per_run: usize,
    pub min_episodic_body_chars: usize,
    pub checkpoint_stale_after_secs: i64,
    pub conflict_lower: f64,
    pub similarity_threshold: f64,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            cron_expr: DEFAULT_CRON_EXPR.to_owned(),
            cron_tz: DEFAULT_CRON_TZ.to_owned(),
            stale_policy: StalePolicy::default(),
            dedup_fts_candidates: DEFAULT_DEDUP_FTS_CANDIDATES,
            max_sessions_per_run: DEFAULT_MAX_SESSIONS_PER_RUN,
            min_episodic_body_chars: DEFAULT_MIN_EPISODIC_BODY_CHARS,
            checkpoint_stale_after_secs: CHECKPOINT_STALE_AFTER_SECS,
            conflict_lower: CONFLICT_LOWER_DEFAULT,
            similarity_threshold: SIMILARITY_THRESHOLD_DEFAULT,
        }
    }
}

impl ConsolidationConfig {
    pub const fn with_min_episodic_body_chars(mut self, value: usize) -> Self {
        self.min_episodic_body_chars = value;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StalePolicy {
    pub episodic_min_age_days: i64,
    pub importance_threshold: f32,
}

impl Default for StalePolicy {
    fn default() -> Self {
        Self {
            episodic_min_age_days: STALE_EPISODIC_MIN_AGE_DAYS,
            importance_threshold: STALE_IMPORTANCE_THRESHOLD,
        }
    }
}
