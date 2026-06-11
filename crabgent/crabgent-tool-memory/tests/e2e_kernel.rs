//! End-to-end test: kernel + scripted provider + memory tool +
//! `AllowAllPolicy` (round-trip works) plus a deny-all variant
//! (kernel returns a soft-error tool result and continues).

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use crabgent_core::error::ProviderError;
use crabgent_core::policy::{AllowAllPolicy, DenyAllPolicy};
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse};
use crabgent_core::{
    ContentBlock, Kernel, Message, ModelInfo, Owner, PolicyHook, RunCtx, RunId, RunRequest, Subject,
};
use crabgent_store::MemoryMemoryStore;
use crabgent_store::traits::MemoryStore;
use crabgent_test_support::{done, tool_call, tool_use};
use crabgent_tool_memory::MemoryTool;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct ScriptedProvider {
    responses: Mutex<Vec<LlmResponse>>,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl ScriptedProvider {
    fn with(responses: Vec<LlmResponse>) -> Self {
        Self::with_recording(responses, Arc::new(Mutex::new(Vec::new())))
    }

    const fn with_recording(
        responses: Vec<LlmResponse>,
        requests: Arc<Mutex<Vec<LlmRequest>>>,
    ) -> Self {
        Self {
            responses: Mutex::new(responses),
            requests,
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.requests
            .lock()
            .expect("mutex should not be poisoned")
            .push(req.clone());
        let mut q = self.responses.lock().expect("mutex should not be poisoned");
        if q.is_empty() {
            return Err(ProviderError::Other("script exhausted".into()));
        }
        Ok(q.remove(0))
    }
    fn name(&self) -> &'static str {
        "scripted"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }
    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("m", "scripted")]
    }
}

fn make_request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("alice"),
        model: "m".into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "remember my address".into(),
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

fn build_tool(policy: Arc<dyn PolicyHook>) -> (MemoryTool, Arc<MemoryMemoryStore>) {
    let store: Arc<MemoryMemoryStore> = Arc::new(MemoryMemoryStore::default());
    let store_dyn: Arc<dyn MemoryStore> = store.clone();
    (MemoryTool::new(store_dyn, policy, None), store)
}

#[tokio::test]
async fn kernel_dispatches_memory_store_then_search_with_allow_all() {
    let (tool, store) = build_tool(Arc::new(AllowAllPolicy));
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::with(vec![
            tool_use(vec![tool_call(
                "c1",
                "memory",
                json!({
                    "op": "store",
                    "scope": {"owner": "alice"},
                    "body": "local-first preferences"
                }),
            )]),
            tool_use(vec![tool_call(
                "c2",
                "memory",
                json!({
                    "op": "search",
                    "scope": {"owner": "alice"},
                    "query": "local-first"
                }),
            )]),
            done("found it"),
        ]))
        .policy(AllowAllPolicy)
        .add_tool(tool)
        .build();
    let text = kernel.run(make_request(), None).await.expect("ok");
    assert_eq!(text, "found it");

    // Side-effect: store now has exactly one alice-scoped doc.
    let q = crabgent_core::SearchQuery::new("local-first")
        .scope(crabgent_core::MemoryScope::for_owner(Owner::new("alice")));
    let hits = store.search(&q).await.expect("test result");
    assert_eq!(hits.len(), 1);
    let hit = hits.first().expect("memory search should return one hit");
    assert!(hit.body.contains("local-first"));
}

#[tokio::test]
async fn kernel_surfaces_permission_deny_as_soft_tool_result() {
    let (tool, _) = build_tool(Arc::new(DenyAllPolicy));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::with_recording(
            vec![
                tool_use(vec![tool_call(
                    "c1",
                    "memory",
                    json!({
                        "op": "store",
                        "scope": {"owner": "alice"},
                        "body": "secret"
                    }),
                )]),
                done("repaired after deny"),
            ],
            requests.clone(),
        ))
        .policy(AllowAllPolicy)
        .add_tool(tool)
        .build();
    let text = kernel
        .run(make_request(), None)
        .await
        .expect("run continues");
    assert_eq!(text, "repaired after deny");

    let recorded = requests.lock().expect("mutex should not be poisoned");
    let second_request = recorded
        .get(1)
        .expect("second provider request should contain tool result");
    let tool_result = second_request
        .messages
        .iter()
        .find(|msg| {
            msg.get("role").and_then(Value::as_str) == Some("tool_result")
                && msg.get("call_id").and_then(Value::as_str) == Some("c1")
        })
        .expect("tool_result message");
    assert_eq!(tool_result["is_error"], json!(true));
    assert!(
        tool_result["output"]
            .as_str()
            .is_some_and(|reason| reason.contains("DenyAllPolicy"))
    );
}

#[tokio::test]
async fn invalid_args_return_soft_tool_result_and_continue() {
    let (tool, _) = build_tool(Arc::new(AllowAllPolicy));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::with_recording(
            vec![
                tool_use(vec![tool_call(
                    "c1",
                    "memory",
                    json!({"op": "search", "query": "x"}),
                )]),
                done("repaired after invalid args"),
            ],
            requests.clone(),
        ))
        .policy(AllowAllPolicy)
        .add_tool(tool)
        .build();
    let text = kernel
        .run(make_request(), None)
        .await
        .expect("run should continue after invalid tool args");
    assert_eq!(text, "repaired after invalid args");

    let recorded = requests.lock().expect("mutex should not be poisoned");
    let second_request = recorded
        .get(1)
        .expect("second provider request should contain invalid-args result");
    let tool_result = second_request
        .messages
        .iter()
        .find(|msg| {
            msg.get("role").and_then(Value::as_str) == Some("tool_result")
                && msg.get("call_id").and_then(Value::as_str) == Some("c1")
        })
        .expect("tool_result message");
    assert_eq!(tool_result["is_error"], json!(true));
    assert!(
        tool_result["output"]
            .as_str()
            .is_some_and(|reason| reason.contains("missing field `scope`"))
    );
}
