//! Configuration for the tool-output compaction hook + recall tool.

use chrono::Duration;

/// Minimum estimated tokens before an output is worth compacting.
pub const DEFAULT_MIN_TOKENS: usize = 4096;
/// Byte ceiling for the compute budget: larger inputs degrade to raw
/// passthrough (the per-tool byte caps remain the floor).
pub const DEFAULT_MAX_INPUT_BYTES: usize = 1_048_576;
/// Line ceiling for the compute budget.
pub const DEFAULT_MAX_INPUT_LINES: usize = 50_000;
/// JSON nesting depth cap for the MCP structured-key summary walk.
pub const DEFAULT_MAX_JSON_DEPTH: usize = 64;
/// Recalls of one tool within a run before compaction self-disables for it.
pub const DEFAULT_AUTODISABLE_N: u32 = 3;
/// Lifetime of a stashed full output, in hours.
pub const DEFAULT_TTL_HOURS: i64 = 24;
/// Default byte cap returned by a single `recall` call when no limit is given.
pub const DEFAULT_RECALL_LIMIT: usize = 4 * 1024;
/// Hard byte cap a single `recall` call will ever return. Larger payloads
/// paginate via `offset`/`limit`; the cap exists so the model cannot pull a
/// full payload back and neutralize the compaction.
pub const MAX_RECALL_LIMIT: usize = 32 * 1024;

/// Default stash lifetime as a [`Duration`].
#[must_use]
pub const fn default_ttl() -> Duration {
    Duration::hours(DEFAULT_TTL_HOURS)
}

/// Tunables for [`crate::hook::ToolCompactHook`] and
/// [`crate::recall::RecallTool`].
#[derive(Debug, Clone)]
pub struct ToolCompactConfig {
    /// Compact only outputs estimated above this token count.
    pub min_tokens: usize,
    /// Inputs larger than this byte count degrade to raw passthrough.
    pub max_input_bytes: usize,
    /// Inputs with more lines than this degrade to raw passthrough.
    pub max_input_lines: usize,
    /// JSON nesting depth cap for the MCP filter walk.
    pub max_json_depth: usize,
    /// Recall count per (run, tool) that disables compaction for that tool.
    pub autodisable_n: u32,
    /// Lifetime of a stashed full output.
    pub ttl: Duration,
    /// Default byte cap for a `recall` slice when the caller omits `limit`.
    pub recall_default_limit: usize,
    /// Maximum byte cap a single `recall` call will return.
    pub recall_max_limit: usize,
}

impl Default for ToolCompactConfig {
    fn default() -> Self {
        Self {
            min_tokens: DEFAULT_MIN_TOKENS,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            max_input_lines: DEFAULT_MAX_INPUT_LINES,
            max_json_depth: DEFAULT_MAX_JSON_DEPTH,
            autodisable_n: DEFAULT_AUTODISABLE_N,
            ttl: default_ttl(),
            recall_default_limit: DEFAULT_RECALL_LIMIT,
            recall_max_limit: MAX_RECALL_LIMIT,
        }
    }
}

impl ToolCompactConfig {
    /// Override the compaction token threshold.
    #[must_use]
    pub const fn with_min_tokens(mut self, tokens: usize) -> Self {
        self.min_tokens = tokens;
        self
    }

    /// Override the recall count that self-disables compaction per tool.
    #[must_use]
    pub const fn with_autodisable_n(mut self, n: u32) -> Self {
        self.autodisable_n = n;
        self
    }

    /// Override the stash lifetime.
    #[must_use]
    pub const fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_constants() {
        let cfg = ToolCompactConfig::default();
        assert_eq!(cfg.min_tokens, DEFAULT_MIN_TOKENS);
        assert_eq!(cfg.autodisable_n, DEFAULT_AUTODISABLE_N);
        assert_eq!(cfg.recall_max_limit, MAX_RECALL_LIMIT);
        assert_eq!(cfg.ttl, Duration::hours(24));
    }

    #[test]
    fn builders_override_fields() {
        let cfg = ToolCompactConfig::default()
            .with_min_tokens(10)
            .with_autodisable_n(1)
            .with_ttl(Duration::seconds(5));
        assert_eq!(cfg.min_tokens, 10);
        assert_eq!(cfg.autodisable_n, 1);
        assert_eq!(cfg.ttl, Duration::seconds(5));
    }
}
