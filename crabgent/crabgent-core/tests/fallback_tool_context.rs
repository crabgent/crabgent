//! Fallback attempt context tests for tool dispatch.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, EffortSource, EventStream, Kernel, LlmRequest, LlmResponse,
    Message, ModelInfo, ModelTarget, Provider, ProviderCapabilities, ProviderError,
    ReasoningEffort, RunCtx, RunId, RunRequest, StopReason, Subject, Tool, ToolCtx, ToolError,
    Usage,
};
use futures::stream;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct FailingProvider {
    calls: Arc<AtomicUsize>,
}

struct ToolCallingProvider {
    calls: Arc<AtomicUsize>,
    model_info: ModelInfo,
    request_efforts: Arc<std::sync::Mutex<Vec<Option<ReasoningEffort>>>>,
}

struct RetryableStreamProvider {
    model_info: ModelInfo,
    request_efforts: Arc<std::sync::Mutex<Vec<Option<ReasoningEffort>>>>,
}

struct ContextCaptureTool {
    seen: Arc<std::sync::Mutex<Option<ObservedToolCtx>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedToolCtx {
    model: String,
    effort: Option<ReasoningEffort>,
    source: EffortSource,
}

#[async_trait]
impl Provider for FailingProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::Api {
            status: 503,
            message: "upstream unavailable".into(),
            retry_after_secs: None,
        })
    }

    fn name(&self) -> &'static str {
        "primary"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        let mut info = ModelInfo::minimal("primary-model", "primary");
        info.caps.reasoning_effort = Some(ReasoningEffort::Low);
        vec![info]
    }
}

#[async_trait]
impl Provider for ToolCallingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.request_efforts
            .lock()
            .expect("request efforts mutex not poisoned")
            .push(req.reasoning_effort);
        let call_count = self.calls.fetch_add(1, Ordering::SeqCst);
        if call_count == 0 {
            return Ok(LlmResponse {
                text: String::new(),
                tool_calls: vec![crabgent_core::ToolCall {
                    id: "fallback-tool-call".to_owned(),
                    name: "capture_context".to_owned(),
                    args: json!({}),
                    thought_signature: None,
                }],
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                model: req.model.clone(),
            });
        }
        Ok(LlmResponse {
            text: "done".to_owned(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "fallback"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![self.model_info.clone()]
    }
}

#[async_trait]
impl Provider for RetryableStreamProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Other("stream-only retry provider".into()))
    }

    async fn stream(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<EventStream, ProviderError> {
        self.request_efforts
            .lock()
            .expect("request efforts mutex not poisoned")
            .push(req.reasoning_effort);
        Ok(Box::pin(stream::iter(vec![Err(
            ProviderError::RetryableStream {
                message: "fallback stream reset".into(),
            },
        )])))
    }

    fn name(&self) -> &'static str {
        "plain-fallback"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![self.model_info.clone()]
    }
}

#[async_trait]
impl Tool for ContextCaptureTool {
    fn name(&self) -> &'static str {
        "capture_context"
    }

    fn description(&self) -> &'static str {
        "captures the current model context"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let Some(model) = ctx.current_model.as_ref() else {
            return Err(ToolError::Execution("missing current model".into()));
        };
        let Some(effort) = ctx.current_effort else {
            return Err(ToolError::Execution("missing current effort".into()));
        };
        let observed = ObservedToolCtx {
            model: model.info.id.as_str().to_owned(),
            effort: effort.effort,
            source: effort.source,
        };
        *self.seen.lock().expect("seen mutex not poisoned") = Some(observed);
        Ok(json!({"ok": true}))
    }
}

fn fallback_model_info(id: &str, provider: &str, effort: Option<ReasoningEffort>) -> ModelInfo {
    let mut info = ModelInfo::minimal(id, provider.to_owned());
    info.caps.reasoning_effort = effort;
    info
}

fn request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("user"),
        model: ModelTarget::id("primary-model"),
        explicit_model: None,
        session_model_override: None,
        fallbacks: vec![ModelTarget::new("fallback", "fallback-model")],
        messages: vec![Message::User {
            content: vec![ContentBlock::Text { text: "hi".into() }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(2),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

#[tokio::test]
async fn fallback_tool_call_uses_fallback_attempt_context() {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(std::sync::Mutex::new(None));
    let fallback_efforts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(FailingProvider {
            calls: Arc::clone(&primary_calls),
        })
        .provider(ToolCallingProvider {
            calls: Arc::clone(&fallback_calls),
            model_info: fallback_model_info(
                "fallback-model",
                "fallback",
                Some(ReasoningEffort::Medium),
            ),
            request_efforts: Arc::clone(&fallback_efforts),
        })
        .add_tool(ContextCaptureTool {
            seen: Arc::clone(&seen),
        })
        .policy(AllowAllPolicy)
        .build();

    let text = kernel.run(request(), None).await.expect("test result");

    assert_eq!(text, "done");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 2);
    let observed = seen
        .lock()
        .expect("seen mutex not poisoned")
        .clone()
        .expect("tool should record context");
    assert_eq!(
        observed,
        ObservedToolCtx {
            model: "fallback-model".to_owned(),
            effort: Some(ReasoningEffort::Medium),
            source: EffortSource::ModelDefault,
        }
    );
}

#[tokio::test]
async fn fallback_tool_call_uses_none_effort_when_fallback_lacks_support() {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(std::sync::Mutex::new(None));
    let fallback_efforts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(FailingProvider {
            calls: Arc::clone(&primary_calls),
        })
        .provider(ToolCallingProvider {
            calls: Arc::clone(&fallback_calls),
            model_info: fallback_model_info("fallback-model", "fallback", None),
            request_efforts: Arc::clone(&fallback_efforts),
        })
        .add_tool(ContextCaptureTool {
            seen: Arc::clone(&seen),
        })
        .policy(AllowAllPolicy)
        .build();

    let mut req = request();
    req.reasoning_effort = Some(ReasoningEffort::High);
    let text = kernel.run(req, None).await.expect("test result");

    assert_eq!(text, "done");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        fallback_efforts
            .lock()
            .expect("fallback efforts mutex not poisoned")
            .as_slice(),
        &[None, None],
    );
    let observed = seen
        .lock()
        .expect("seen mutex not poisoned")
        .clone()
        .expect("tool should record context");
    assert_eq!(
        observed,
        ObservedToolCtx {
            model: "fallback-model".to_owned(),
            effort: None,
            source: EffortSource::ModelDefault,
        }
    );
}

#[tokio::test]
async fn pump_retry_restores_resolved_effort_source_after_plain_fallback() {
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let plain_efforts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let supported_efforts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::new(std::sync::Mutex::new(None));
    let kernel = Kernel::builder()
        .provider(FailingProvider {
            calls: Arc::clone(&primary_calls),
        })
        .provider(RetryableStreamProvider {
            model_info: fallback_model_info("plain-fallback-model", "plain-fallback", None),
            request_efforts: Arc::clone(&plain_efforts),
        })
        .provider(ToolCallingProvider {
            calls: Arc::clone(&fallback_calls),
            model_info: fallback_model_info(
                "fallback-model",
                "fallback",
                Some(ReasoningEffort::Low),
            ),
            request_efforts: Arc::clone(&supported_efforts),
        })
        .add_tool(ContextCaptureTool {
            seen: Arc::clone(&seen),
        })
        .policy(AllowAllPolicy)
        .build();

    let mut req = request();
    req.reasoning_effort = Some(ReasoningEffort::High);
    req.fallbacks = vec![
        ModelTarget::new("plain-fallback", "plain-fallback-model"),
        ModelTarget::new("fallback", "fallback-model"),
    ];
    let text = kernel.run(req, None).await.expect("test result");

    assert_eq!(text, "done");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        plain_efforts
            .lock()
            .expect("plain efforts mutex not poisoned")
            .as_slice(),
        &[None, None],
    );
    assert_eq!(
        supported_efforts
            .lock()
            .expect("supported efforts mutex not poisoned")
            .as_slice(),
        &[Some(ReasoningEffort::High), Some(ReasoningEffort::High)],
    );
    let observed = seen
        .lock()
        .expect("seen mutex not poisoned")
        .clone()
        .expect("tool should record context");
    assert_eq!(
        observed,
        ObservedToolCtx {
            model: "fallback-model".to_owned(),
            effort: Some(ReasoningEffort::High),
            source: EffortSource::Explicit,
        }
    );
}
