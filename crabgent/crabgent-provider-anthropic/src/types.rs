//! Configuration and internal types for the Anthropic provider.

use std::time::Duration;

use thiserror::Error;

pub const EXTENDED_CACHE_TTL_BETA: &str = "extended-cache-ttl-2025-04-11";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TtlError {
    #[error("anthropic cache_ttl must be \"5m\" or \"1h\", got: {0}")]
    Invalid(String),
}

/// All tunables for the Anthropic HTTP client.
///
/// `endpoint` defaults to `https://api.anthropic.com`. `max_retries` is
/// the number of retries AFTER the first attempt (so a value of 3 means
/// up to 4 total attempts). `retry_base_delay` is multiplied by
/// `2^attempt` per retry, jittered, and capped at 30s.
#[derive(Clone)]
pub struct AnthropicConfig {
    pub api_key: String, // bare String, header-safety via api_key_is_header_safe()
    pub endpoint: String,
    pub anthropic_version: String,
    pub max_retries: u32,
    pub retry_base_delay: Duration,
    /// Per-attempt request timeout. The retry lifecycle applies this to each
    /// individual attempt, so the worst-case total wall time of a `complete`
    /// call is roughly `(max_retries + 1) * complete_timeout` plus backoff.
    /// There is intentionally no outer total-duration cap (mirrors the
    /// `crabgent-provider-openai` sibling); a consumer that needs a hard ceiling
    /// wraps the call itself.
    pub complete_timeout: Duration,
    /// Anthropic beta header values. Use [`Self::with_betas`] so cache TTL
    /// support keeps its required beta.
    pub(crate) anthropic_betas: Vec<String>,
    /// Prompt cache TTL. `Some("5m")` or `Some("1h")` enables cache control;
    /// `None` disables cache control. Use [`Self::with_cache_ttl`] to keep
    /// beta-header invariants in sync.
    pub(crate) cache_ttl: Option<String>,
}

impl std::fmt::Debug for AnthropicConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicConfig")
            .field("api_key", &"sk-ant-****")
            .field("endpoint", &self.endpoint)
            .field("anthropic_version", &self.anthropic_version)
            .field("max_retries", &self.max_retries)
            .field("retry_base_delay", &self.retry_base_delay)
            .field("complete_timeout", &self.complete_timeout)
            .field("anthropic_betas", &self.anthropic_betas)
            .field("cache_ttl", &self.cache_ttl)
            .finish()
    }
}

impl AnthropicConfig {
    /// Default endpoint for the Anthropic API.
    pub const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com";
    /// API version pinned to the stable Messages API release.
    pub const DEFAULT_VERSION: &str = "2023-06-01";

    /// Build a config with sensible defaults from an API key.
    ///
    /// Prompt caching is enabled by default with `cache_ttl = Some("5m")`.
    /// Use [`Self::with_cache_ttl`] with `None` to opt out.
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            endpoint: Self::DEFAULT_ENDPOINT.to_string(),
            anthropic_version: Self::DEFAULT_VERSION.to_string(),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(500),
            complete_timeout: Duration::from_mins(2),
            anthropic_betas: vec![EXTENDED_CACHE_TTL_BETA.to_string()],
            cache_ttl: Some("5m".to_string()),
        }
    }

    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    #[must_use]
    pub const fn with_max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    #[must_use]
    pub const fn with_retry_base_delay(mut self, d: Duration) -> Self {
        self.retry_base_delay = d;
        self
    }

    #[must_use]
    pub const fn with_complete_timeout(mut self, d: Duration) -> Self {
        self.complete_timeout = d;
        self
    }

    #[must_use]
    pub fn cache_ttl(&self) -> Option<&str> {
        self.cache_ttl.as_deref()
    }

    #[must_use]
    pub fn anthropic_betas(&self) -> &[String] {
        &self.anthropic_betas
    }

    /// Merge additional beta-header entries into the configuration,
    /// preserving insertion order and skipping duplicates already
    /// present (including the auto-added `EXTENDED_CACHE_TTL_BETA`).
    /// Prior callers using `with_betas(Vec::new())` to clear the list
    /// must instead rebuild a fresh `AnthropicConfig::new(..)` and
    /// reapply the desired non-beta settings; a public clear-all path
    /// is intentionally no longer offered.
    #[must_use]
    pub fn with_betas(mut self, betas: Vec<String>) -> Self {
        for beta in betas {
            if !self.anthropic_betas.contains(&beta) {
                self.anthropic_betas.push(beta);
            }
        }
        self.ensure_cache_beta_if_needed();
        self
    }

    pub fn with_cache_ttl(mut self, ttl: Option<String>) -> Result<Self, TtlError> {
        if let Some(value) = ttl.as_deref()
            && !matches!(value, "5m" | "1h")
        {
            return Err(TtlError::Invalid(value.to_string()));
        }
        self.cache_ttl = ttl;
        self.ensure_cache_beta_if_needed();
        Ok(self)
    }

    fn ensure_cache_beta_if_needed(&mut self) {
        if self.cache_ttl.is_some()
            && !self
                .anthropic_betas
                .iter()
                .any(|beta| beta == EXTENDED_CACHE_TTL_BETA)
        {
            self.anthropic_betas
                .push(EXTENDED_CACHE_TTL_BETA.to_string());
        }
    }
}

/// Reject API keys containing characters not legal in HTTP header values.
/// Catches accidental newline / control-char injection before reqwest panics.
pub(crate) fn api_key_is_header_safe(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii() && (0x20..=0x7e).contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_new_defaults() {
        let c = AnthropicConfig::new("sk-ant-api03-xxx");
        assert_eq!(c.endpoint, AnthropicConfig::DEFAULT_ENDPOINT);
        assert_eq!(c.anthropic_version, AnthropicConfig::DEFAULT_VERSION);
        assert_eq!(c.max_retries, 3);
        assert_eq!(c.retry_base_delay, Duration::from_millis(500));
        assert_eq!(c.complete_timeout, Duration::from_mins(2));
    }

    #[test]
    fn config_debug_masks_api_key() {
        let c = AnthropicConfig::new("secret-test-ant-key-99999");

        let formatted = format!("{c:?}");

        assert!(!formatted.contains("secret-test-ant-key-99999"));
        assert!(formatted.contains("sk-ant-****"));
    }

    #[test]
    fn cache_ttl_default_is_some_5m() {
        let c = AnthropicConfig::new("k");

        assert_eq!(c.cache_ttl, Some("5m".to_string()));
    }

    #[test]
    fn config_new_includes_extended_cache_ttl_beta() {
        let c = AnthropicConfig::new("k");

        assert!(
            c.anthropic_betas
                .iter()
                .any(|beta| beta == "extended-cache-ttl-2025-04-11")
        );
    }

    #[test]
    fn config_builder_methods_override() {
        let c = AnthropicConfig::new("k")
            .with_endpoint("http://localhost")
            .with_max_retries(7)
            .with_retry_base_delay(Duration::from_millis(10))
            .with_complete_timeout(Duration::from_secs(5))
            .with_betas(vec!["beta-x".into()]);
        assert_eq!(c.endpoint, "http://localhost");
        assert_eq!(c.max_retries, 7);
        assert_eq!(c.retry_base_delay, Duration::from_millis(10));
        assert_eq!(c.complete_timeout, Duration::from_secs(5));
        assert_eq!(
            c.anthropic_betas,
            vec![EXTENDED_CACHE_TTL_BETA.to_string(), "beta-x".to_string()]
        );
    }

    #[test]
    fn with_cache_ttl_rejects_2h() {
        let err = AnthropicConfig::new("k")
            .with_cache_ttl(Some("2h".to_string()))
            .expect_err("expected error");

        assert_eq!(err, TtlError::Invalid("2h".to_string()));
    }

    #[test]
    fn with_cache_ttl_accepts_5m_1h_none() {
        for (ttl, expected) in [
            (Some("5m"), Some("5m")),
            (Some("1h"), Some("1h")),
            (None, None),
        ] {
            let c = AnthropicConfig::new("k")
                .with_cache_ttl(ttl.map(str::to_string))
                .expect("valid ttl");

            assert_eq!(c.cache_ttl.as_deref(), expected);
        }
    }

    #[test]
    fn with_cache_ttl_adds_beta_idempotent() {
        let c = AnthropicConfig::new("k")
            .with_cache_ttl(Some("5m".to_string()))
            .expect("valid ttl")
            .with_cache_ttl(Some("5m".to_string()))
            .expect("valid ttl");

        let count = c
            .anthropic_betas
            .iter()
            .filter(|beta| beta.as_str() == EXTENDED_CACHE_TTL_BETA)
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn with_cache_ttl_none_keeps_beta() {
        let c = AnthropicConfig::new("k")
            .with_cache_ttl(None)
            .expect("none disables caching");

        assert!(
            c.anthropic_betas
                .iter()
                .any(|beta| beta == EXTENDED_CACHE_TTL_BETA)
        );
    }

    #[test]
    fn with_betas_preserves_cache_ttl_beta_when_cache_enabled() {
        let c = AnthropicConfig::new("k").with_betas(vec!["beta-x".into()]);

        assert_eq!(
            c.anthropic_betas,
            vec![EXTENDED_CACHE_TTL_BETA.to_string(), "beta-x".to_string()]
        );
    }

    #[test]
    fn with_betas_empty_vec_is_noop_under_merge() {
        // Merge-semantics: with_betas no longer clears prior betas. An
        // empty vector is a no-op and the auto-added cache beta from
        // `new()` survives even when caching was disabled afterwards.
        let c = AnthropicConfig::new("k")
            .with_cache_ttl(None)
            .expect("none disables caching")
            .with_betas(Vec::new());

        assert!(
            c.anthropic_betas
                .iter()
                .any(|beta| beta == EXTENDED_CACHE_TTL_BETA)
        );
    }

    #[test]
    fn with_betas_merge_keeps_cache_ttl_beta_with_explicit_ttl() {
        let cfg = AnthropicConfig::new("k")
            .with_cache_ttl(Some("5m".to_string()))
            .expect("valid ttl")
            .with_betas(vec!["my-beta".into()]);

        assert!(cfg.anthropic_betas.contains(&"my-beta".to_string()));
        assert!(
            cfg.anthropic_betas
                .contains(&EXTENDED_CACHE_TTL_BETA.to_string())
        );
    }

    #[test]
    fn with_betas_merge_is_dedup_idempotent() {
        let cfg = AnthropicConfig::new("k")
            .with_betas(vec!["my-beta".into()])
            .with_betas(vec!["my-beta".into()]);

        let count = cfg
            .anthropic_betas
            .iter()
            .filter(|beta| beta.as_str() == "my-beta")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn with_betas_merge_accumulates_distinct_entries() {
        let cfg = AnthropicConfig::new("k")
            .with_betas(vec!["alpha".into()])
            .with_betas(vec!["beta".into()]);

        assert!(cfg.anthropic_betas.contains(&"alpha".to_string()));
        assert!(cfg.anthropic_betas.contains(&"beta".to_string()));
        assert!(
            cfg.anthropic_betas
                .contains(&EXTENDED_CACHE_TTL_BETA.to_string())
        );
    }

    #[test]
    fn header_safe_accepts_printable_ascii() {
        assert!(api_key_is_header_safe("sk-ant-api03-AbC123"));
        assert!(api_key_is_header_safe("k"));
    }

    #[test]
    fn header_safe_rejects_control_chars() {
        assert!(!api_key_is_header_safe(""));
        assert!(!api_key_is_header_safe("with\rnewline"));
        assert!(!api_key_is_header_safe("with\nnewline"));
        assert!(!api_key_is_header_safe("with\ttab"));
        assert!(!api_key_is_header_safe("with\u{00}null"));
        assert!(!api_key_is_header_safe("with\u{7f}del"));
    }

    #[test]
    fn header_safe_rejects_non_ascii() {
        assert!(!api_key_is_header_safe("with-ümlaut"));
    }
}
