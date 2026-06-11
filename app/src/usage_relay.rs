//! Process-global per-run token-usage relay.
//!
//! Token [`Usage`] never reaches the `run_streaming` event stream; it only
//! rides on the `after_llm` hook. The TUI needs per-turn token counts for its
//! status line, so [`UsageRelayHook`] accumulates each run's usage into a
//! process-global map keyed by `RunId`, and the TUI WebSocket bridge drains
//! it with [`take`] once the turn completes and forwards it as a `usage`
//! frame.
//!
//! The hook only records runs whose subject id starts with `tui:` (the TUI
//! bridge sets `Subject::new("tui:<agent>")`). Channel runs (Matrix,
//! Telegram, cron) are ignored so the map cannot grow unbounded from runs the
//! TUI never drains.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use async_trait::async_trait;
use crabgent_core::{Decision, Hook, LlmRequest, LlmResponse, RunCtx, RunId};

/// Subject-id prefix the TUI bridge uses; only these runs are tracked.
const TUI_SUBJECT_PREFIX: &str = "tui:";

/// Accumulated token usage for one run (summed across its LLM calls).
#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_field_names)] // canonical token field names
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
}

fn registry() -> &'static Mutex<HashMap<RunId, TurnUsage>> {
    static REG: OnceLock<Mutex<HashMap<RunId, TurnUsage>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drain and return the accumulated usage for `run_id`. Called by the TUI
/// bridge after a turn ends, which also frees the map entry.
#[must_use]
pub fn take(run_id: &RunId) -> Option<TurnUsage> {
    registry().lock().ok()?.remove(run_id)
}

/// Hook that sums `after_llm` usage per TUI run into the global registry.
pub struct UsageRelayHook;

#[async_trait]
impl Hook for UsageRelayHook {
    async fn after_llm(
        &self,
        _req: &LlmRequest,
        resp: &LlmResponse,
        ctx: &RunCtx,
    ) -> Decision<LlmResponse> {
        if ctx.subject.id().starts_with(TUI_SUBJECT_PREFIX)
            && let Ok(mut map) = registry().lock()
        {
            let entry = map.entry(ctx.run_id.clone()).or_default();
            entry.input_tokens += u64::from(resp.usage.input_tokens);
            entry.output_tokens += u64::from(resp.usage.output_tokens);
            entry.cache_read_tokens += u64::from(resp.usage.cache_read_tokens);
        }
        Decision::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_returns_none_for_unknown_run() {
        let id = RunId::new();
        assert!(take(&id).is_none());
    }

    #[tokio::test]
    async fn after_llm_accumulates_only_tui_subjects() {
        use crabgent_core::{ModelId, StopReason, Subject, Usage};

        let hook = UsageRelayHook;
        let resp = LlmResponse {
            text: String::new(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 100,
                output_tokens: 20,
                cache_creation_tokens: 0,
                cache_read_tokens: 5,
            },
            model: ModelId::new("m"),
        };
        let req = LlmRequest {
            model: ModelId::new("m"),
            system_prompt: None,
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            stop_sequences: vec![],
            reasoning_effort: None,
            web_search: crabgent_core::WebSearchConfig::default(),
            tool_choice: None,
        };

        // A non-TUI subject is ignored.
        let chan = RunId::new();
        let ctx_chan = RunCtx::new(chan.clone(), Subject::new("matrix:@x"));
        let _ = hook.after_llm(&req, &resp, &ctx_chan).await;
        assert!(take(&chan).is_none());

        // A TUI subject accumulates and drains once.
        let tui = RunId::new();
        let ctx_tui = RunCtx::new(tui.clone(), Subject::new("tui:local"));
        let _ = hook.after_llm(&req, &resp, &ctx_tui).await;
        let _ = hook.after_llm(&req, &resp, &ctx_tui).await;
        let u = take(&tui).expect("usage recorded for tui run");
        assert_eq!(u.input_tokens, 200);
        assert_eq!(u.output_tokens, 40);
        assert_eq!(u.cache_read_tokens, 10);
        assert!(take(&tui).is_none(), "second take drains the entry");
    }
}
