//! Per-agent `reasoning_effort` override hook.
//!
//! Reads the configured value once at construction; on every `before_llm`
//! it sets `LlmRequest::reasoning_effort = Some(value)` only when the
//! request does not already carry an explicit effort. This gives per-turn
//! UI/API overrides precedence over the agent default, while still winning
//! over the upstream model-capability default that `request_for_attempt`
//! would otherwise fill in.
//!
//! The host passes the model ids that advertise `reasoning_effort`.
//! OpenAI-compatible routers often reject unknown request fields, so
//! unsupported models are left untouched.

use std::collections::HashSet;

use async_trait::async_trait;
use crabgent_core::{Decision, Hook, LlmRequest, ReasoningEffort, RunCtx};

pub struct ReasoningEffortHook {
    effort: ReasoningEffort,
    supported_models: Option<HashSet<String>>,
}

impl ReasoningEffortHook {
    #[must_use]
    pub const fn new(effort: ReasoningEffort) -> Self {
        Self {
            effort,
            supported_models: None,
        }
    }

    #[must_use]
    pub fn with_supported_models(mut self, models: impl IntoIterator<Item = String>) -> Self {
        self.supported_models = Some(models.into_iter().collect());
        self
    }

    /// Parse a config string (`"low"` / `"medium"` / `"high"`,
    /// case-insensitive). Returns `None` on empty input or unknown
    /// values so the caller can surface a config warning instead of
    /// silently accepting garbage.
    #[must_use]
    pub fn parse(raw: &str) -> Option<ReasoningEffort> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "low" => Some(ReasoningEffort::Low),
            "medium" => Some(ReasoningEffort::Medium),
            "high" => Some(ReasoningEffort::High),
            _ => None,
        }
    }
}

#[async_trait]
impl Hook for ReasoningEffortHook {
    async fn before_llm(&self, req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        if req.reasoning_effort.is_some() {
            return Decision::Continue;
        }
        if self
            .supported_models
            .as_ref()
            .is_some_and(|models| !models.contains(req.model.as_str()))
        {
            return Decision::Continue;
        }
        let mut next = req.clone();
        next.reasoning_effort = Some(self.effort);
        Decision::Replace(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::{LlmRequest, RunId, Subject};

    fn ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("u"))
    }

    fn req(effort: Option<ReasoningEffort>) -> LlmRequest {
        LlmRequest {
            model: "m".into(),
            system_prompt: None,
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            stop_sequences: vec![],
            reasoning_effort: effort,
            web_search: crabgent_core::WebSearchConfig::default(),
            tool_choice: None,
        }
    }

    #[test]
    fn parse_accepts_three_levels_case_insensitive() {
        assert_eq!(
            ReasoningEffortHook::parse("low"),
            Some(ReasoningEffort::Low)
        );
        assert_eq!(
            ReasoningEffortHook::parse("MEDIUM"),
            Some(ReasoningEffort::Medium)
        );
        assert_eq!(
            ReasoningEffortHook::parse("  High  "),
            Some(ReasoningEffort::High)
        );
    }

    #[test]
    fn parse_rejects_unknown_values() {
        assert_eq!(ReasoningEffortHook::parse(""), None);
        assert_eq!(ReasoningEffortHook::parse("ultra"), None);
        assert_eq!(ReasoningEffortHook::parse("none"), None);
    }

    #[tokio::test]
    async fn before_llm_overrides_none() {
        let hook = ReasoningEffortHook::new(ReasoningEffort::High);
        let r = req(None);
        match hook.before_llm(&r, &ctx()).await {
            Decision::Replace(next) => {
                assert_eq!(next.reasoning_effort, Some(ReasoningEffort::High));
            }
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn before_llm_keeps_explicit_low_over_configured_medium() {
        let hook = ReasoningEffortHook::new(ReasoningEffort::Medium);
        let r = req(Some(ReasoningEffort::Low));
        assert!(matches!(
            hook.before_llm(&r, &ctx()).await,
            Decision::Continue
        ));
    }

    #[tokio::test]
    async fn before_llm_continue_when_already_at_target() {
        let hook = ReasoningEffortHook::new(ReasoningEffort::High);
        let r = req(Some(ReasoningEffort::High));
        assert!(matches!(
            hook.before_llm(&r, &ctx()).await,
            Decision::Continue
        ));
    }

    #[tokio::test]
    async fn before_llm_skips_models_outside_supported_set() {
        let hook = ReasoningEffortHook::new(ReasoningEffort::High)
            .with_supported_models(["gpt-5.5".to_owned()]);
        let r = req(None);
        assert!(matches!(
            hook.before_llm(&r, &ctx()).await,
            Decision::Continue
        ));
    }
}
