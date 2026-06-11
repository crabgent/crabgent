use std::collections::HashMap;

use chrono::Duration;
use thiserror::Error;

pub(crate) const CACHE_READ_TOOL_NAME: &str = "cache_read";
pub const DEFAULT_MIN_TOKENS: usize = 4096;
pub const DEFAULT_PREVIEW_BYTES: usize = 256;
pub const DEFAULT_TTL_HOURS: i64 = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCacheConfig {
    pub min_tokens: usize,
    pub tool_overrides: HashMap<String, usize>,
}

impl Default for ToolCacheConfig {
    fn default() -> Self {
        Self {
            min_tokens: DEFAULT_MIN_TOKENS,
            tool_overrides: HashMap::new(),
        }
    }
}

pub(crate) const fn default_ttl() -> Duration {
    Duration::hours(DEFAULT_TTL_HOURS)
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ToolCacheConfigError {
    #[error("cache_read threshold override is forbidden")]
    CacheReadOverrideForbidden,
}
