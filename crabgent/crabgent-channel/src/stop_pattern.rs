//! Stop-pattern matching for channel inbound text.
//!
//! Custom pattern lists replace the defaults entirely; they do not merge.

use regex::Regex;

use crate::error::ChannelError;

const DEFAULT_PATTERNS: &[&str] = &[r"(?i)^(stop|stopp|cancel|abbruch|halt)!?$"];

#[derive(Debug)]
pub struct StopPatternMatcher {
    patterns: Vec<Regex>,
}

impl StopPatternMatcher {
    pub fn new(patterns: Vec<String>) -> Result<Self, ChannelError> {
        let compiled = patterns
            .into_iter()
            .map(|pattern| Regex::new(pattern.trim()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { patterns: compiled })
    }

    #[must_use]
    pub const fn empty() -> Self {
        Self {
            patterns: Vec::new(),
        }
    }

    #[must_use]
    pub fn matches(&self, body: &str) -> bool {
        self.patterns
            .iter()
            .any(|pattern| pattern.is_match(body.trim()))
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

impl Default for StopPatternMatcher {
    fn default() -> Self {
        let patterns = DEFAULT_PATTERNS
            .iter()
            .map(|pattern| Regex::new(pattern).expect("default stop pattern must compile"))
            .collect();
        Self { patterns }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_compiles() {
        let matcher = StopPatternMatcher::default();
        assert!(!matcher.is_empty());
    }

    #[test]
    fn default_matches_all_5_words_case_insensitive() {
        let matcher = StopPatternMatcher::default();
        for word in ["STOP", "stopp", "Cancel", "abbruch", "Halt!"] {
            assert!(matcher.matches(word), "{word} should match");
        }
    }

    #[test]
    fn default_does_not_match_normal_text() {
        let matcher = StopPatternMatcher::default();
        assert!(!matcher.matches("please stop after this answer"));
    }

    #[test]
    fn override_patterns_compile_and_match() {
        let matcher = StopPatternMatcher::new(vec!["^pause$".to_owned()])
            .expect("override pattern should compile");
        assert!(matcher.matches("pause"));
        assert!(!matcher.matches("stop"));
    }

    #[test]
    fn invalid_regex_returns_err() {
        let err = StopPatternMatcher::new(vec!["[unbalanced".to_owned()])
            .expect_err("invalid regex should fail");
        assert!(matches!(err, ChannelError::InvalidPattern(_)));
    }

    #[test]
    fn empty_constructor_is_empty() {
        let matcher = StopPatternMatcher::empty();
        assert!(matcher.is_empty());
        assert!(!matcher.matches("stop"));
    }

    #[test]
    fn empty_patterns_via_new_never_matches() {
        let matcher = StopPatternMatcher::new(Vec::new()).expect("empty patterns should compile");
        assert!(matcher.is_empty());
        assert!(!matcher.matches("stop"));
    }

    #[test]
    fn trim_whitespace() {
        let matcher = StopPatternMatcher::default();
        assert!(matcher.matches("  stop  "));
    }
}
