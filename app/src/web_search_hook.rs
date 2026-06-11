//! Per-agent hosted web-search toggle.
//!
//! `KernelChannelInbox::build_request_with_subject` hardcodes
//! `web_search: WebSearchConfig::default()` (disabled). This hook flips
//! `enabled = true` and optionally sets `max_uses` for agents that opt
//! in via `Agent.web_search = true` in config.
//!
//! The hosted search runs server-side inside the provider. The kernel
//! surfaces results via `Event::ServerToolResult`; nothing further to do
//! on the host side.

use std::collections::HashSet;

use async_trait::async_trait;
use crabgent_core::{Decision, Hook, LlmRequest, RunCtx};

pub struct WebSearchHook {
    enabled: bool,
    max_uses: Option<u32>,
    /// Lower-case model ids known to support hosted web search. When
    /// empty, every model is accepted (back-compat). When non-empty,
    /// the hook keeps `web_search.enabled = false` for ids outside the
    /// set so fallback runs on web-search-incapable models don't fail the
    /// kernel pre-flight.
    supported_models: HashSet<String>,
}

impl WebSearchHook {
    #[must_use]
    pub fn new(enabled: bool, max_uses: Option<u32>) -> Self {
        Self {
            enabled,
            max_uses,
            supported_models: HashSet::new(),
        }
    }

    #[must_use]
    pub fn with_supported_models(mut self, ids: impl IntoIterator<Item = String>) -> Self {
        self.supported_models = ids.into_iter().map(|s| s.to_ascii_lowercase()).collect();
        self
    }
}

#[async_trait]
impl Hook for WebSearchHook {
    async fn before_llm(&self, req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        if !self.enabled {
            return Decision::Continue;
        }
        if !self.supported_models.is_empty()
            && !self
                .supported_models
                .contains(&req.model.as_str().to_ascii_lowercase())
        {
            // Fallback model lacks hosted web search; clear the flag so
            // the kernel pre-flight does not return WebSearchUnsupported.
            if !req.web_search.enabled {
                return Decision::Continue;
            }
            let mut next = req.clone();
            next.web_search.enabled = false;
            return Decision::Replace(next);
        }
        if req.web_search.enabled && req.web_search.max_uses == self.max_uses {
            return Decision::Continue;
        }
        let mut next = req.clone();
        next.web_search.enabled = true;
        if let Some(max) = self.max_uses {
            next.web_search.max_uses = Some(max);
        }
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

    fn req() -> LlmRequest {
        LlmRequest {
            model: "m".into(),
            system_prompt: None,
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            stop_sequences: vec![],
            reasoning_effort: None,
            web_search: crabgent_core::WebSearchConfig::default(),
            tool_choice: None,
        }
    }

    #[tokio::test]
    async fn disabled_hook_is_passthrough() {
        let hook = WebSearchHook::new(false, None);
        assert!(matches!(
            hook.before_llm(&req(), &ctx()).await,
            Decision::Continue
        ));
    }

    #[tokio::test]
    async fn enabled_hook_flips_request_flag() {
        let hook = WebSearchHook::new(true, None);
        match hook.before_llm(&req(), &ctx()).await {
            Decision::Replace(next) => assert!(next.web_search.enabled),
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn enabled_hook_propagates_max_uses() {
        let hook = WebSearchHook::new(true, Some(5));
        match hook.before_llm(&req(), &ctx()).await {
            Decision::Replace(next) => {
                assert!(next.web_search.enabled);
                assert_eq!(next.web_search.max_uses, Some(5));
            }
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn enabled_hook_idempotent_when_already_set() {
        let hook = WebSearchHook::new(true, Some(3));
        let mut r = req();
        r.web_search.enabled = true;
        r.web_search.max_uses = Some(3);
        assert!(matches!(
            hook.before_llm(&r, &ctx()).await,
            Decision::Continue
        ));
    }
}
