//! End-to-end test: kernel + scripted provider + session-search tool +
//! `AllowAllPolicy` (search returns hits) plus a deny-all variant
//! (kernel returns a soft-error tool result and continues).

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_core::error::ProviderError;
use crabgent_core::policy::{AllowAllPolicy, DenyAllPolicy};
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse};
use crabgent_core::{
    ContentBlock, Kernel, MemoryScope, Message, ModelInfo, Owner, PolicyHook, RunCtx, RunId,
    RunRequest, Subject,
};
use crabgent_store::SessionId;
use crabgent_store::memory::MemorySessionStore;
use crabgent_store::records::Session;
use crabgent_store::traits::SessionStore;
use crabgent_test_support::{done, tool_call, tool_use};
use crabgent_tool_session::SessionSearchTool;
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
                text: "look up my last conversation".into(),
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

async fn make_tool_with_session(
    policy: Arc<dyn PolicyHook>,
    owner: &str,
    body: &str,
) -> (SessionSearchTool, Arc<MemorySessionStore>) {
    let store: Arc<MemorySessionStore> = Arc::new(MemorySessionStore::default());
    let store_dyn: Arc<dyn SessionStore> = store.clone();
    let now = Utc::now();
    let session = Session {
        id: SessionId::new(),
        owner: Owner::new(owner),
        scope: MemoryScope::for_owner(Owner::new(owner)),
        thread: None,
        title: None,
        summary: None,
        compaction_summary: None,
        model_override: None,
        reasoning_effort_override: None,
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: body.to_owned(),
            }],
            timestamp: None,
        }],
        created_at: now,
        updated_at: now,
    };
    store.save(&session).await.expect("test result");
    (SessionSearchTool::new(store_dyn, policy), store)
}

#[tokio::test]
async fn kernel_dispatches_session_search_with_allow_all() {
    let (tool, _) = make_tool_with_session(
        Arc::new(AllowAllPolicy),
        "alice",
        "we agreed on the migration plan yesterday",
    )
    .await;
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::with(vec![
            tool_use(vec![tool_call(
                "c1",
                "session_search",
                json!({
                    "scope": {"owner": "alice"},
                    "query": "migration"
                }),
            )]),
            done("retrieved"),
        ]))
        .policy(AllowAllPolicy)
        .add_tool(tool)
        .build();
    let text = kernel.run(make_request(), None).await.expect("ok");
    assert_eq!(text, "retrieved");
}

#[tokio::test]
async fn deny_all_blocks_session_search() {
    let (tool, _) = make_tool_with_session(Arc::new(DenyAllPolicy), "alice", "blocked").await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::with_recording(
            vec![
                tool_use(vec![tool_call(
                    "c1",
                    "session_search",
                    json!({
                        "scope": {"owner": "alice"},
                        "query": "x"
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
async fn missing_scope_returns_soft_tool_result_and_continues() {
    let (tool, _) = make_tool_with_session(Arc::new(AllowAllPolicy), "alice", "x").await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::with_recording(
            vec![
                tool_use(vec![tool_call(
                    "c1",
                    "session_search",
                    json!({"query": "x"}),
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
            .is_some_and(|reason| reason.contains("scope required"))
    );
}
