//! Provider-specific model capability extension marker.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Marker trait for provider-specific metadata stored in
/// [`ModelInfo::extensions`](crate::model::ModelInfo::extensions).
///
/// Values are stored as `Arc<dyn Any + Send + Sync>` and retrieved by
/// concrete type through [`ModelInfo::capability`](crate::model::ModelInfo::capability).
pub trait ModelCapability: Send + Sync + 'static {}

/// Reasoning-effort hint for models that expose explicit reasoning depth.
///
/// Anthropic Sonnet-thinking, `OpenAI` o-family and gpt-5.x carry separate
/// reasoning budgets; this enum advertises the kernel-side default per model.
/// Providers map it to their wire-specific shape (Responses uses
/// `{"effort": ..., "summary": "auto"}`, Chat Completions uses
/// `reasoning_effort: "none"|"low"|"medium"|"high"|"xhigh"`).
///
/// Variants are ordered by ascending effort. Not every model accepts every
/// level: gpt-5.2+ added `"xhigh"` and dropped the legacy `"minimal"`, and the
/// Codex backend rejects `"none"` for some codex models. The provider forwards
/// the value verbatim and the backend validates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// Reasoning disabled. Wire value `"none"` (distinct from the legacy
    /// `"minimal"`). As an override this is `Some(ReasoningEffort::Disabled)`,
    /// whereas an outer `Option::None` means "no override set".
    #[serde(rename = "none")]
    Disabled,
    Low,
    Medium,
    High,
    XHigh,
}

impl ReasoningEffort {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }
}

/// Error returned when an unknown reasoning-effort string is parsed.
#[derive(Debug, thiserror::Error)]
#[error("unknown reasoning effort: {0}")]
pub struct ParseReasoningEffortError(pub String);

impl FromStr for ReasoningEffort {
    type Err = ParseReasoningEffortError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Self::Disabled),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            other => Err(ParseReasoningEffortError(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_effort_serializes_lowercase() {
        let cases = [
            (ReasoningEffort::Disabled, "\"none\""),
            (ReasoningEffort::Low, "\"low\""),
            (ReasoningEffort::Medium, "\"medium\""),
            (ReasoningEffort::High, "\"high\""),
            (ReasoningEffort::XHigh, "\"xhigh\""),
        ];
        for (effort, expected) in cases {
            let json = serde_json::to_string(&effort).expect("ser");
            assert_eq!(json, expected);
            let back: ReasoningEffort = serde_json::from_str(&json).expect("de");
            assert_eq!(effort, back);
        }
    }

    #[test]
    fn reasoning_effort_as_str_matches_serde() {
        assert_eq!(ReasoningEffort::Disabled.as_str(), "none");
        assert_eq!(ReasoningEffort::Low.as_str(), "low");
        assert_eq!(ReasoningEffort::Medium.as_str(), "medium");
        assert_eq!(ReasoningEffort::High.as_str(), "high");
        assert_eq!(ReasoningEffort::XHigh.as_str(), "xhigh");
    }

    #[test]
    fn reasoning_effort_from_str_accepts_lowercase_only() {
        assert_eq!(
            "none".parse::<ReasoningEffort>().expect("none"),
            ReasoningEffort::Disabled
        );
        assert_eq!(
            "xhigh".parse::<ReasoningEffort>().expect("xhigh"),
            ReasoningEffort::XHigh
        );
        assert_eq!(
            "low".parse::<ReasoningEffort>().expect("low"),
            ReasoningEffort::Low
        );
        assert_eq!(
            "LOW"
                .parse::<ReasoningEffort>()
                .expect_err("uppercase effort is rejected")
                .0,
            "LOW"
        );
    }
}
