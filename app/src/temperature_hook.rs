//! Tool-aware temperature default.
//!
//! Stamps `LlmRequest::temperature` only when the upstream/inbox left it
//! `None`:
//! - tools present  -> 0.0 (greedy: stabilise tool selection)
//! - tools empty    -> 0.7 (sampled: keep prose natural)

use async_trait::async_trait;
use crabgent_core::{Decision, Hook, LlmRequest, RunCtx};

const TEMP_WITH_TOOLS: f32 = 0.0;
const TEMP_WITHOUT_TOOLS: f32 = 0.7;

pub struct TemperatureHook;

impl TemperatureHook {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for TemperatureHook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Hook for TemperatureHook {
    async fn before_llm(&self, req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        if req.temperature.is_some() {
            return Decision::Continue;
        }
        let temp = if req.tools.is_empty() {
            TEMP_WITHOUT_TOOLS
        } else {
            TEMP_WITH_TOOLS
        };
        let mut next = req.clone();
        next.temperature = Some(temp);
        Decision::Replace(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::{LlmRequest, RunId, Subject, ToolDef};

    fn ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("u"))
    }

    fn req(tools: Vec<ToolDef>, temperature: Option<f32>) -> LlmRequest {
        LlmRequest {
            model: "m".into(),
            system_prompt: None,
            messages: vec![],
            tools,
            max_tokens: None,
            temperature,
            stop_sequences: vec![],
            reasoning_effort: None,
            web_search: crabgent_core::WebSearchConfig::default(),
            tool_choice: None,
        }
    }

    fn tool() -> ToolDef {
        ToolDef {
            name: "t".into(),
            description: "d".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[tokio::test]
    async fn stamps_zero_when_tools_present_and_temp_none() {
        let hook = TemperatureHook::new();
        let r = req(vec![tool()], None);
        match hook.before_llm(&r, &ctx()).await {
            Decision::Replace(next) => assert_eq!(next.temperature, Some(TEMP_WITH_TOOLS)),
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stamps_seven_when_no_tools_and_temp_none() {
        let hook = TemperatureHook::new();
        let r = req(vec![], None);
        match hook.before_llm(&r, &ctx()).await {
            Decision::Replace(next) => assert_eq!(next.temperature, Some(TEMP_WITHOUT_TOOLS)),
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn continues_when_temperature_already_set() {
        let hook = TemperatureHook::new();
        let r = req(vec![tool()], Some(0.42));
        assert!(matches!(
            hook.before_llm(&r, &ctx()).await,
            Decision::Continue
        ));
    }
}
