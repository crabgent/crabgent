//! Provider-specific tool advertisement limit coverage.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, Decision, Hook, Kernel, KernelError, LlmRequest, LlmResponse,
    Message, ModelInfo, ModelTarget, Provider, ProviderCapabilities, ProviderError, RunCtx, RunId,
    RunRequest, StopReason, Subject, Tool, ToolCtx, ToolError, Usage,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct LimitedProvider {
    name: &'static str,
    model: &'static str,
    text: &'static str,
    calls: Arc<AtomicUsize>,
    tool_limit: usize,
}

struct SwitchModelHook {
    target: &'static str,
}

#[async_trait]
impl Hook for SwitchModelHook {
    async fn before_llm(&self, req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        let mut req = req.clone();
        req.model = self.target.into();
        Decision::Replace(req)
    }
}

#[async_trait]
impl Provider for LimitedProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            text: self.text.to_owned(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal(self.model, self.name)]
    }

    fn tool_advertise_limit(&self) -> Option<usize> {
        Some(self.tool_limit)
    }
}

struct NamedTool(&'static str);

#[async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &'static str {
        self.0
    }

    fn description(&self) -> &'static str {
        "test tool"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(json!({"ok": true}))
    }
}

fn provider(
    name: &'static str,
    model: &'static str,
    text: &'static str,
    tool_limit: usize,
) -> (LimitedProvider, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        LimitedProvider {
            name,
            model,
            text,
            calls: Arc::clone(&calls),
            tool_limit,
        },
        calls,
    )
}

fn request(model: &str) -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("user"),
        model: ModelTarget::id(model),
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
async fn provider_qualified_model_uses_selected_provider_tool_limit() {
    let (primary, primary_calls) = provider("primary", "primary-model", "wrong", 1);
    let (fallback, fallback_calls) = provider("fallback", "fallback-model", "from fallback", 2);
    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .add_tool(NamedTool("first"))
        .add_tool(NamedTool("second"))
        .build();
    let mut req = request("primary-model");
    req.model = ModelTarget::new("fallback", "fallback-model");

    let text = kernel.run(req, None).await.expect("run should pass");

    assert_eq!(text, "from fallback");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unqualified_model_id_uses_registry_owner_tool_limit() {
    let (primary, primary_calls) = provider("primary", "primary-model", "wrong", 1);
    let (fallback, fallback_calls) = provider("fallback", "fallback-model", "from fallback", 2);
    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .add_tool(NamedTool("first"))
        .add_tool(NamedTool("second"))
        .build();

    let text = kernel
        .run(request("fallback-model"), None)
        .await
        .expect("unqualified model should use owning provider limit");

    assert_eq!(text, "from fallback");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unqualified_model_id_rejects_above_registry_owner_tool_limit() {
    let (primary, primary_calls) = provider("primary", "primary-model", "wrong", 1);
    let (fallback, fallback_calls) = provider("fallback", "fallback-model", "from fallback", 2);
    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .add_tool(NamedTool("first"))
        .add_tool(NamedTool("second"))
        .add_tool(NamedTool("third"))
        .build();

    let err = kernel
        .run(request("fallback-model"), None)
        .await
        .expect_err("three tools should exceed fallback provider limit");

    assert!(matches!(
        err,
        KernelError::TooManyTools { count: 3, max: 2 }
    ));
    assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn before_llm_model_rewrite_uses_final_provider_tool_limit() {
    let (primary, primary_calls) = provider("primary", "primary-model", "wrong", 3);
    let (fallback, fallback_calls) = provider("fallback", "fallback-model", "from fallback", 2);
    let kernel = Kernel::builder()
        .provider(primary)
        .provider(fallback)
        .policy(AllowAllPolicy)
        .add_hook(SwitchModelHook {
            target: "fallback-model",
        })
        .add_tool(NamedTool("first"))
        .add_tool(NamedTool("second"))
        .add_tool(NamedTool("third"))
        .build();

    let err = kernel
        .run(request("primary-model"), None)
        .await
        .expect_err("hook-selected provider limit must reject three tools");

    assert!(matches!(
        err,
        KernelError::TooManyTools { count: 3, max: 2 }
    ));
    assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}
