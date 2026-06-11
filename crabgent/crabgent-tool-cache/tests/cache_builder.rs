use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::hook::{Decision, Hook, RunCtx};
use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, LlmRequest, LlmResponse, Message, ModelInfo, Provider,
    ProviderCapabilities, ProviderError, RunId, RunRequest, StopReason, Subject, Tool, ToolCtx,
    ToolError, ToolResult, Usage,
};
use crabgent_store::memory::MemoryToolCacheStore;
use crabgent_store::{SessionId, ToolCacheStore};
use crabgent_test_support::{done_for_model, tool_call};
use crabgent_tool_cache::{
    KernelBuilderExt, ToolCacheBuilder, ToolCacheConfig, ToolCacheConfigError, ToolCacheHook,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const BIG_OUTPUT: &str = "abcdefghijklmnopqrstuvwxyz";

struct CacheRoundTripProvider {
    step: Mutex<u8>,
    observed_content: Arc<Mutex<Option<String>>>,
    observed_cache_id: Arc<Mutex<Option<String>>>,
}

impl CacheRoundTripProvider {
    const fn new(
        observed_content: Arc<Mutex<Option<String>>>,
        observed_cache_id: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            step: Mutex::new(0),
            observed_content,
            observed_cache_id,
        }
    }
}

#[async_trait]
impl Provider for CacheRoundTripProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let mut step = self.step.lock().expect("mutex should not be poisoned");
        let response = match *step {
            0 => calling_tool("large_output", "large-1", json!({})),
            1 => {
                let cache_id = extract_cache_id(req);
                *self
                    .observed_cache_id
                    .lock()
                    .expect("mutex should not be poisoned") = Some(cache_id.clone());
                calling_tool("cache_read", "read-1", json!({ "id": cache_id }))
            }
            2 => {
                *self
                    .observed_content
                    .lock()
                    .expect("mutex should not be poisoned") = Some(extract_cache_read_content(req));
                done("read cached")
            }
            _ => return Err(ProviderError::Other("script exhausted".into())),
        };
        *step += 1;
        Ok(response)
    }

    fn name(&self) -> &'static str {
        "cache-test"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("test", "cache-test")]
    }
}

struct LargeOutputTool;

#[async_trait]
impl Tool for LargeOutputTool {
    fn name(&self) -> &'static str {
        "large_output"
    }

    fn description(&self) -> &'static str {
        "returns output large enough to cache"
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(json!(BIG_OUTPUT))
    }
}

#[tokio::test]
async fn hook_and_tool_share_session_id_via_default_resolver() {
    let store = Arc::new(MemoryToolCacheStore::default());
    let observed_content = Arc::new(Mutex::new(None));
    let observed_cache_id = Arc::new(Mutex::new(None));
    let kernel = kernel_with_cache(
        Arc::clone(&store),
        observed_content.clone(),
        observed_cache_id,
        None,
    );

    let text = kernel.run(make_request(), None).await.expect("run");

    assert_eq!(text, "read cached");
    assert_eq!(
        observed_content
            .lock()
            .expect("mutex should not be poisoned")
            .as_deref(),
        Some(BIG_OUTPUT)
    );
}

#[tokio::test]
async fn builder_custom_resolver_is_shared_by_hook_and_tool() {
    let store = Arc::new(MemoryToolCacheStore::default());
    let custom_session = SessionId::new();
    let observed_content = Arc::new(Mutex::new(None));
    let observed_cache_id = Arc::new(Mutex::new(None));
    let kernel = kernel_with_cache(
        Arc::clone(&store),
        observed_content.clone(),
        Arc::clone(&observed_cache_id),
        Some(custom_session.clone()),
    );

    kernel.run(make_request(), None).await.expect("run");

    let cache_id = observed_cache_id
        .lock()
        .expect("mutex should not be poisoned")
        .clone()
        .expect("cache id");
    let entry = store
        .get(&cache_id, &custom_session)
        .await
        .expect("store get")
        .expect("cached entry");
    assert_eq!(entry.content, BIG_OUTPUT);
    assert_eq!(
        observed_content
            .lock()
            .expect("mutex should not be poisoned")
            .as_deref(),
        Some(BIG_OUTPUT)
    );
}

#[tokio::test]
async fn per_tool_override_replaces_default() {
    let store = Arc::new(MemoryToolCacheStore::default());
    let hook = ToolCacheHook::new(Arc::clone(&store))
        .with_config(ToolCacheConfig {
            min_tokens: 1024,
            tool_overrides: HashMap::from([("large_output".to_owned(), 1)]),
        })
        .with_preview_bytes(4);
    let ctx = RunCtx::new(RunId::new(), Subject::new("override-user"));
    let call = tool_call("override-1", "large_output", json!({}));
    let result = ToolResult::success(json!("short output")).with_call_id("override-1");

    let dec = hook.after_tool(&call, &result, &ctx).await;

    assert!(matches!(dec, Decision::Replace(_)));
}

#[test]
fn cache_read_in_override_map_rejected_at_build_time() {
    let store = Arc::new(MemoryToolCacheStore::default());
    let Err(err) = ToolCacheBuilder::new(store).with_tool_override("cache_read", 0) else {
        panic!("expected cache_read override rejection");
    };

    assert_eq!(err, ToolCacheConfigError::CacheReadOverrideForbidden);
}

#[test]
fn with_tool_cache_kernel_builder_wires_hook_and_tool() {
    let store = Arc::new(MemoryToolCacheStore::default());
    let observed_content = Arc::new(Mutex::new(None));
    let observed_cache_id = Arc::new(Mutex::new(None));

    let kernel = Kernel::builder()
        .provider(CacheRoundTripProvider::new(
            observed_content,
            observed_cache_id,
        ))
        .policy(AllowAllPolicy)
        .add_tool(LargeOutputTool)
        .with_tool_cache(store)
        .build();

    assert_eq!(kernel.hook_count(), 1);
    assert!(kernel.tool("cache_read").is_some());
}

fn kernel_with_cache(
    store: Arc<MemoryToolCacheStore>,
    observed_content: Arc<Mutex<Option<String>>>,
    observed_cache_id: Arc<Mutex<Option<String>>>,
    custom_session: Option<SessionId>,
) -> Kernel {
    let builder = Kernel::builder()
        .provider(CacheRoundTripProvider::new(
            observed_content,
            observed_cache_id,
        ))
        .policy(AllowAllPolicy)
        .add_tool(LargeOutputTool);
    let cache = ToolCacheBuilder::new(store)
        .with_min_tokens(1)
        .with_preview_bytes(4);
    let cache = match custom_session {
        Some(session) => cache.with_session_resolver(move |_| session.clone()),
        None => cache,
    };
    cache.install(builder).build()
}

fn make_request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("integration-user"),
        model: "test".into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "please run the large tool".into(),
            }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(5),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
    }
}

fn calling_tool(name: &str, id: &str, args: Value) -> LlmResponse {
    LlmResponse {
        text: String::new(),
        tool_calls: vec![tool_call(id, name, args)],
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        model: "test".into(),
    }
}

fn done(text: &str) -> LlmResponse {
    done_for_model(text, "test")
}

fn extract_cache_id(req: &LlmRequest) -> String {
    wrapped_tool_output(req, "large-1", "large_output")
        .get("cache_id")
        .and_then(Value::as_str)
        .expect("cache id")
        .to_owned()
}

fn extract_cache_read_content(req: &LlmRequest) -> String {
    wrapped_tool_output(req, "read-1", "cache_read")
        .get("content")
        .and_then(Value::as_str)
        .expect("cache_read content")
        .to_owned()
}

fn wrapped_tool_output(req: &LlmRequest, call_id: &str, tool_name: &str) -> Value {
    let output = req
        .messages
        .iter()
        .find(|msg| msg.get("call_id").and_then(Value::as_str) == Some(call_id))
        .and_then(|msg| msg.get("output"))
        .and_then(Value::as_str)
        .expect("wrapped tool output");
    let prefix = format!("<tool_output tool=\"{tool_name}\">");
    let body = output
        .strip_prefix(&prefix)
        .and_then(|s| s.strip_suffix("</tool_output>"))
        .expect("tool output envelope");

    serde_json::from_str(body).expect("tool output json")
}
