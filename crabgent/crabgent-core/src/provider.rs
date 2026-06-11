//! Provider trait, capability descriptor, and streaming event type.

use std::pin::Pin;

use async_trait::async_trait;
use futures::stream::{self, Stream};
use tokio_util::sync::CancellationToken;

use crate::error::ProviderError;
use crate::hook::RunCtx;
use crate::model::ModelInfo;
use crate::types::{LlmRequest, LlmResponse, StopReason, ToolCall, Usage};

/// Capability advertisement returned by `Provider::capabilities`.
///
/// `streaming = true` means the provider's `stream()` is native. The
/// default `Provider::stream` impl synthesises a stream from `complete()`,
/// which works for non-streaming use cases but does not deliver real-time
/// deltas. Callers that need real-time output should check this flag.
// Independent feature flags, not a state machine: enum or bitflags would
// reduce readability without modeling any constraint.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent provider capability flags are clearer than a bitmask"
)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub streaming: bool,
    pub tools: bool,
    pub vision: bool,
    /// Provider can accept audio input on at least one model. The
    /// pre-flight gate also requires the routed model's
    /// `ModelCapabilities::supports_audio` to be true; setting this flag
    /// alone is not sufficient to send audio.
    pub audio: bool,
    pub system_prompt: bool,
    pub thinking: bool,
    pub prompt_cache: bool,
    /// Provider supports hosted web-search tool (e.g. Anthropic
    /// `web_search_20250305`, `OpenAI` `web_search_preview`).
    pub web_search: bool,
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
}

/// An event emitted by the provider during a `stream()` call.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ProviderEvent {
    /// A text fragment from the assistant.
    TextDelta(String),
    /// A reasoning fragment from the assistant (Anthropic
    /// `thinking_delta`, `OpenAI` Responses `reasoning_text.delta` and
    /// `reasoning_summary_text.delta`). The kernel dispatches this as
    /// [`Event::Reasoning`](crate::hook::Event::Reasoning).
    ReasoningDelta(String),
    /// A complete tool-use block from the model.
    ToolUse(ToolCall),
    /// Token usage update.
    Usage(Usage),
    /// The model finished its turn.
    Stop(StopReason),
    /// The provider executed a server-side tool (e.g. hosted web search) and
    /// returned its result block. The kernel echoes `Message::ProviderBlock`
    /// back into the conversation for multi-turn correlation, and surfaces
    /// `Event::ServerToolResult` to hooks.
    ///
    /// `name` is the provider tool name (e.g. `"web_search_20250305"`).
    /// `content` is the verbatim provider result JSON.
    /// `citations` is parsed from the provider block when available.
    ServerToolResult {
        provider: String,
        name: String,
        content: serde_json::Value,
        citations: Vec<crate::types::Citation>,
    },
}

/// Stream of provider events returned by `Provider::stream`.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send>>;

/// Abstraction over LLM backends.
///
/// Implementors handle authentication, request body formatting, response
/// parsing, and retries. The kernel can route fallback between registered
/// provider/model targets after a provider returns an eligible error.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Non-streaming chat request.
    ///
    /// `ctx` carries the run/session identity providers may use to scope
    /// per-conversation caches (e.g. Codex prompt caching by `session_id`).
    /// Providers that do not need conversation identity ignore the parameter.
    async fn complete(
        &self,
        req: &LlmRequest,
        ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError>;

    /// Streaming chat request. Default impl wraps `complete()` in a
    /// synthetic event stream. Native streaming providers override.
    async fn stream(
        &self,
        req: &LlmRequest,
        ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<EventStream, ProviderError> {
        let resp = self.complete(req, ctx, cancel).await?;
        Ok(Box::pin(stream::iter(synthesize_events(resp))))
    }

    /// Provider name for logging and metrics.
    fn name(&self) -> &'static str;

    /// Capability advertisement.
    ///
    /// This is the provider-wide envelope (broadest superset of all
    /// supported models). Per-model capabilities live on the
    /// [`ModelInfo`] entries returned by [`Self::models`].
    fn capabilities(&self) -> ProviderCapabilities;

    /// Maximum number of tools the provider accepts in one advertised
    /// request. `None` asks the kernel to use its provider-neutral
    /// default.
    fn tool_advertise_limit(&self) -> Option<usize> {
        None
    }

    /// List of models the provider can serve.
    ///
    /// `KernelBuilder::try_build()` validates these into a
    /// [`ModelRegistry`] and rejects unknown ids in `LlmRequest.model`
    /// fail-closed. Default returns an empty `Vec`; any provider that
    /// wants to be wired into a kernel must override with at least one
    /// model. Empty model lists are rejected at registration-time.
    ///
    /// [`ModelRegistry`]: crate::model::ModelRegistry
    fn models(&self) -> Vec<ModelInfo> {
        Vec::new()
    }

    /// Fetch the provider's current model list.
    ///
    /// Default returns the static [`Self::models`] catalog. Providers with
    /// model-discovery endpoints can override and map discovery failures to
    /// [`ProviderError::ModelDiscovery`].
    async fn fetch_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(self.models())
    }
}

fn synthesize_events(resp: LlmResponse) -> Vec<Result<ProviderEvent, ProviderError>> {
    let mut events = Vec::with_capacity(2 + resp.tool_calls.len() + 1);
    if !resp.text.is_empty() {
        events.push(Ok(ProviderEvent::TextDelta(resp.text)));
    }
    for call in resp.tool_calls {
        events.push(Ok(ProviderEvent::ToolUse(call)));
    }
    events.push(Ok(ProviderEvent::Usage(resp.usage)));
    events.push(Ok(ProviderEvent::Stop(resp.stop_reason)));
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolCall;
    use futures::StreamExt;
    use serde_json::json;

    struct EchoProvider;

    #[async_trait]
    impl Provider for EchoProvider {
        async fn complete(
            &self,
            req: &LlmRequest,
            _ctx: &RunCtx,
            _cancel: Option<&CancellationToken>,
        ) -> Result<LlmResponse, ProviderError> {
            Ok(LlmResponse {
                text: format!("got {} messages", req.messages.len()),
                tool_calls: vec![],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: req.model.clone(),
            })
        }

        fn name(&self) -> &'static str {
            "echo"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                streaming: true,
                tools: true,
                ..Default::default()
            }
        }

        fn models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo::minimal("test", "echo")]
        }
    }

    fn make_req() -> LlmRequest {
        LlmRequest {
            model: "test".into(),
            system_prompt: None,
            messages: vec![json!({"role": "user", "content": "hi"})],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            stop_sequences: vec![],
            reasoning_effort: None,
            web_search: crate::types::WebSearchConfig::default(),
            tool_choice: None,
        }
    }

    #[test]
    fn default_tool_advertise_limit_is_provider_neutral() {
        let provider = EchoProvider;
        assert_eq!(provider.tool_advertise_limit(), None);
    }

    #[tokio::test]
    async fn provider_fetch_models_default_returns_models() {
        let provider = EchoProvider;
        let models = provider.fetch_models().await.expect("models");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id.as_str(), "test");
        assert_eq!(models[0].provider, "echo");
    }

    fn test_ctx() -> RunCtx {
        RunCtx::new(crate::RunId::new(), crate::Subject::new("test-subject"))
    }

    #[tokio::test]
    async fn complete_echoes_message_count() {
        let p = EchoProvider;
        let ctx = test_ctx();
        let r = p.complete(&make_req(), &ctx, None).await.expect("ok");
        assert_eq!(r.text, "got 1 messages");
        assert_eq!(r.stop_reason, StopReason::EndTurn);
    }

    #[tokio::test]
    async fn default_stream_wraps_complete() {
        let p = EchoProvider;
        let ctx = test_ctx();
        let mut s = p.stream(&make_req(), &ctx, None).await.expect("stream ok");
        let mut events = Vec::new();
        while let Some(ev) = s.next().await {
            events.push(ev.expect("event ok"));
        }
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ProviderEvent::TextDelta(_)))
        );
        assert!(events.iter().any(|e| matches!(e, ProviderEvent::Usage(_))));
        let last = events.last().expect("at least one event");
        assert!(matches!(last, ProviderEvent::Stop(_)));
    }

    #[test]
    fn capabilities_default_is_all_off() {
        let c = ProviderCapabilities::default();
        assert!(!c.streaming);
        assert!(!c.tools);
        assert!(!c.audio);
        assert_eq!(c.max_input_tokens, 0);
        assert_eq!(c.max_output_tokens, 0);
    }

    #[test]
    fn synthesize_events_skips_empty_text() {
        let resp = LlmResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "x".into(),
                args: json!({}),
                thought_signature: None,
            }],
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            model: "m".into(),
        };
        let events = synthesize_events(resp);
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[0].as_ref().expect("test result"),
            ProviderEvent::ToolUse(_)
        ));
        assert!(matches!(
            events[1].as_ref().expect("test result"),
            ProviderEvent::Usage(_)
        ));
        assert!(matches!(
            events[2].as_ref().expect("test result"),
            ProviderEvent::Stop(StopReason::ToolUse)
        ));
    }

    #[test]
    fn synthesize_events_includes_text_when_present() {
        let resp = LlmResponse {
            text: "hello".into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: "m".into(),
        };
        let events = synthesize_events(resp);
        assert_eq!(events.len(), 3);
        assert!(
            matches!(events[0].as_ref().expect("test result"), ProviderEvent::TextDelta(s) if s == "hello")
        );
    }

    #[test]
    fn provider_name_is_static() {
        let p = EchoProvider;
        assert_eq!(p.name(), "echo");
    }

    #[test]
    fn provider_capabilities_carries_flags() {
        let p = EchoProvider;
        let c = p.capabilities();
        assert!(c.streaming);
        assert!(c.tools);
    }
}
