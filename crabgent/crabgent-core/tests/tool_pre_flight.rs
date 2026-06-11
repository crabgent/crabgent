use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, KernelError, LlmRequest, LlmResponse, Message,
    ModelCapabilities, ModelId, ModelInfo, ModelTarget, Provider, ProviderCapabilities,
    ProviderError, RunCtx, RunId, RunRequest, StopReason, Subject, Tool, ToolCtx, ToolError, Usage,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct ToolProvider {
    provider_tools: bool,
    model_tools: bool,
    model_aliases: Vec<ModelId>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for ToolProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            text: "ok".into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "tool-test"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: self.provider_tools,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        let mut info = ModelInfo::minimal("m", self.name().to_owned());
        info.aliases.clone_from(&self.model_aliases);
        info.caps = ModelCapabilities {
            supports_tools: self.model_tools,
            ..info.caps
        };
        vec![info]
    }
}

struct NoopTool;

#[async_trait]
impl Tool for NoopTool {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn description(&self) -> &'static str {
        "stub"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(json!({"ok": true}))
    }
}

fn kernel(
    provider_tools: bool,
    model_tools: bool,
    advertise_tool: bool,
) -> (Kernel, Arc<AtomicUsize>) {
    kernel_with_aliases(provider_tools, model_tools, advertise_tool, Vec::new())
}

fn kernel_with_aliases(
    provider_tools: bool,
    model_tools: bool,
    advertise_tool: bool,
    model_aliases: Vec<ModelId>,
) -> (Kernel, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = ToolProvider {
        provider_tools,
        model_tools,
        model_aliases,
        calls: Arc::clone(&calls),
    };
    let builder = Kernel::builder().provider(provider).policy(AllowAllPolicy);
    let kernel = if advertise_tool {
        builder.add_tool(NoopTool).build()
    } else {
        builder.build()
    };
    (kernel, calls)
}

fn request() -> RunRequest {
    request_with_model("m")
}

fn request_with_model(model: impl Into<ModelTarget>) -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("tool-user"),
        model: model.into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text { text: "hi".into() }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(1),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

#[tokio::test]
async fn tool_request_no_tool_provider_rejects() {
    let (kernel, calls) = kernel(false, true, true);

    let err = kernel
        .run(request(), None)
        .await
        .expect_err("provider without tools rejects advertised tools");

    match err {
        KernelError::Provider(ProviderError::ToolsUnsupported { provider, model }) => {
            assert_eq!(provider, "tool-test");
            assert_eq!(model, "m");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn tool_request_no_tool_model_rejects() {
    let (kernel, calls) = kernel(true, false, true);

    let err = kernel
        .run(request(), None)
        .await
        .expect_err("model without tools rejects advertised tools");

    match err {
        KernelError::Provider(ProviderError::ToolsUnsupported { provider, model }) => {
            assert_eq!(provider, "tool-test");
            assert_eq!(model, "m");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn tool_request_with_caps_passes() {
    let (kernel, calls) = kernel(true, true, true);

    let text = kernel.run(request(), None).await.expect("tools ok");

    assert_eq!(text, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn tool_check_resolves_via_model_alias() {
    let (kernel, calls) = kernel_with_aliases(true, true, true, vec![ModelId::new("m-alias")]);

    let text = kernel
        .run(request_with_model("m-alias"), None)
        .await
        .expect("tool alias ok");

    assert_eq!(text, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn no_advertised_tools_unaffected() {
    let (kernel, calls) = kernel(false, false, false);

    let text = kernel.run(request(), None).await.expect("text ok");

    assert_eq!(text, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
