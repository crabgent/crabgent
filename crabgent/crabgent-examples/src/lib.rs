//! Shared helpers for the runnable example binaries.
//!
//! Examples are intentionally minimal: a stub provider, no-op tools,
//! and the simplest possible policy. The point is to show the kernel
//! shape, not a production setup.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_core::{
    LlmRequest, LlmResponse, ModelInfo, Provider, ProviderCapabilities, ProviderError, RunCtx,
    StopReason, ToolCall, Usage,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Provider that emits a fixed sequence of `LlmResponse`s, one per
/// `complete()` call. After the sequence is exhausted, every further
/// call returns the last response (so a chatty REPL keeps echoing the
/// last canned answer).
pub struct ScriptedProvider {
    responses: Vec<LlmResponse>,
    cursor: AtomicUsize,
}

impl ScriptedProvider {
    /// Build a scripted provider from a non-empty response sequence.
    ///
    /// Panics if `responses` is empty.
    pub fn new(responses: Vec<LlmResponse>) -> Self {
        assert!(
            !responses.is_empty(),
            "ScriptedProvider needs at least one response",
        );
        Self {
            responses,
            cursor: AtomicUsize::new(0),
        }
    }

    /// Convenience: a provider that always returns the same plain text.
    pub fn echo(text: impl Into<String>) -> Self {
        let resp = LlmResponse {
            text: text.into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: "scripted".into(),
        };
        Self::new(vec![resp])
    }

    /// First call returns a tool-use response; subsequent calls return
    /// `final_text` with `EndTurn`. Lets a single example show a
    /// tool-call round-trip without bringing in a real provider.
    pub fn tool_then_final(
        tool_name: impl Into<String>,
        tool_args: Value,
        final_text: impl Into<String>,
    ) -> Self {
        let tool = LlmResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: tool_name.into(),
                args: tool_args,
                thought_signature: None,
            }],
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            model: "scripted".into(),
        };
        let final_resp = LlmResponse {
            text: final_text.into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: "scripted".into(),
        };
        Self::new(vec![tool, final_resp])
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let idx = self.cursor.fetch_add(1, Ordering::Relaxed);
        let resp_idx = idx.min(self.responses.len() - 1);
        Ok(self.responses[resp_idx].clone())
    }

    fn name(&self) -> &'static str {
        "scripted"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: false,
            tools: true,
            ..Default::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("scripted", "scripted")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::{AllowAllPolicy, Kernel};

    #[test]
    fn scripted_provider_advertises_minimum_model() {
        let provider = ScriptedProvider::echo("ok");
        let models = provider.models();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id.as_str(), "scripted");
        assert_eq!(models[0].provider, "scripted");
    }

    #[test]
    fn scripted_provider_kernel_build_succeeds() {
        let kernel = Kernel::builder()
            .provider(ScriptedProvider::echo("ok"))
            .policy(AllowAllPolicy)
            .try_build()
            .expect("scripted provider should advertise a valid model");

        assert!(kernel.models().get(&"scripted".into()).is_some());
    }
}
