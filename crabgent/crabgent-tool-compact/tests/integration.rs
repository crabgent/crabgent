//! End-to-end tests for tool-output compaction + recall.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::hook::{Decision, Hook, RunCtx};
use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, LlmRequest, LlmResponse, Message, ModelInfo, Provider,
    ProviderCapabilities, ProviderError, RunId, RunRequest, StopReason, Subject, Tool, ToolCtx,
    ToolError, ToolResult, Usage,
};
use crabgent_store::memory::MemoryToolCacheStore;
use crabgent_store::{SessionId, StoreError, ToolCacheEntry, ToolCacheStore};
use crabgent_test_support::{done_for_model, tool_call};
use crabgent_tool_compact::{
    KernelBuilderExt, ToolCompactBuilder, ToolCompactConfig, ToolCompactHook,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// A `read_file`-named tool returning a long multi-line body the compactor
/// will fold (head + tail) while stashing the full content.
struct BigReadFileTool {
    body: String,
}

#[async_trait]
impl Tool for BigReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "test read_file"
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }
    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(Value::String(self.body.clone()))
    }
}

/// Drives: `read_file` -> recall(handle) -> done.
struct RecallProvider {
    step: Mutex<u8>,
    recalled: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl Provider for RecallProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let mut step = self.step.lock().expect("step lock");
        let response = match *step {
            0 => call_tool("read_file", "r1", json!({})),
            1 => {
                // The body fits under MAX_RECALL_LIMIT, so this exercises the
                // full-loop roundtrip. The per-call cap itself is unit-tested in
                // recall.rs (recall_clamps_limit_to_max).
                let handle = extract_handle(&wrapped_text(req, "r1", "read_file"));
                call_tool(
                    "recall",
                    "rc1",
                    json!({ "op": "recall_raw", "handle": handle, "limit": 1_000_000 }),
                )
            }
            2 => {
                let envelope = wrapped_text(req, "rc1", "recall");
                let parsed: Value = serde_json::from_str(&envelope).expect("recall json");
                let content = parsed
                    .get("content")
                    .and_then(Value::as_str)
                    .expect("content")
                    .to_owned();
                *self.recalled.lock().expect("recalled lock") = Some(content);
                done("done")
            }
            _ => return Err(ProviderError::Other("script exhausted".into())),
        };
        *step += 1;
        Ok(response)
    }

    fn name(&self) -> &'static str {
        "recall-test"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }
    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("test", "recall-test")]
    }
}

/// A store whose insert always fails, to exercise the fail-open path.
struct FailingStore;

#[async_trait]
impl ToolCacheStore for FailingStore {
    async fn insert(&self, _entry: &ToolCacheEntry) -> Result<(), StoreError> {
        Err(StoreError::backend("disk full"))
    }
    async fn get(
        &self,
        _id: &str,
        _session_id: &SessionId,
    ) -> Result<Option<ToolCacheEntry>, StoreError> {
        Ok(None)
    }
    async fn cleanup_expired(&self) -> Result<u64, StoreError> {
        Ok(0)
    }
}

fn body_of_lines(count: usize) -> String {
    let lines: Vec<String> = (0..count)
        .map(|i| format!("content line number {i}"))
        .collect();
    let mut out = lines.join("\n");
    out.push_str("\nfinal line");
    out
}

#[test]
fn install_registers_hook_and_recall_tool() {
    let store = Arc::new(MemoryToolCacheStore::default());
    let kernel = Kernel::builder()
        .provider(RecallProvider {
            step: Mutex::new(0),
            recalled: Arc::new(Mutex::new(None)),
        })
        .policy(AllowAllPolicy)
        .add_tool(BigReadFileTool { body: "x".into() })
        .with_tool_compact(store)
        .build();

    assert_eq!(kernel.hook_count(), 1);
    assert!(kernel.tool("recall").is_some());
}

#[tokio::test]
async fn end_to_end_compaction_and_recall() {
    let store = Arc::new(MemoryToolCacheStore::default());
    let recalled = Arc::new(Mutex::new(None));
    let body = body_of_lines(100);

    let builder = Kernel::builder()
        .provider(RecallProvider {
            step: Mutex::new(0),
            recalled: Arc::clone(&recalled),
        })
        .policy(AllowAllPolicy)
        .add_tool(BigReadFileTool { body: body.clone() });
    let kernel = ToolCompactBuilder::new(store)
        .with_min_tokens(1)
        .install(builder)
        .build();

    let text = kernel.run(make_request(), None).await.expect("run");
    assert_eq!(text, "done");

    // The recall round-tripped the full original body.
    assert_eq!(
        recalled.lock().expect("recalled lock").as_deref(),
        Some(body.as_str())
    );
}

#[tokio::test]
async fn fail_open_when_store_insert_errors() {
    let hook = ToolCompactHook::new(
        Arc::new(FailingStore),
        ToolCompactConfig::default().with_min_tokens(1),
    );
    let ctx = RunCtx::new(RunId::new(), Subject::new("user-1"));
    let call = tool_call("r1", "read_file", json!({}));
    let result = ToolResult::success(Value::String(body_of_lines(100))).with_call_id("r1");

    // Insert fails -> hook leaves the raw output inline.
    let decision = hook.after_tool(&call, &result, &ctx).await;
    assert!(matches!(decision, Decision::Continue));
}

#[tokio::test]
async fn budget_degrade_passes_through_raw() {
    let hook = ToolCompactHook::new(
        Arc::new(MemoryToolCacheStore::default()),
        ToolCompactConfig::default().with_min_tokens(1),
    );
    let ctx = RunCtx::new(RunId::new(), Subject::new("user-1"));
    let call = tool_call("r1", "read_file", json!({}));
    // Over the byte ceiling -> degrade to raw passthrough.
    let huge = "x".repeat(ToolCompactConfig::default().max_input_bytes + 1);
    let result = ToolResult::success(Value::String(huge)).with_call_id("r1");

    let decision = hook.after_tool(&call, &result, &ctx).await;
    assert!(matches!(decision, Decision::Continue));
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
                text: "read the file".into(),
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

fn call_tool(name: &str, id: &str, args: Value) -> LlmResponse {
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

/// Unwrap the `<tool_output tool="...">...</tool_output>` envelope for a call.
fn wrapped_text(req: &LlmRequest, call_id: &str, tool_name: &str) -> String {
    let output = req
        .messages
        .iter()
        .find(|msg| msg.get("call_id").and_then(Value::as_str) == Some(call_id))
        .and_then(|msg| msg.get("output"))
        .and_then(Value::as_str)
        .expect("wrapped tool output");
    let prefix = format!("<tool_output tool=\"{tool_name}\">");
    output
        .strip_prefix(&prefix)
        .and_then(|s| s.strip_suffix("</tool_output>"))
        .expect("tool output envelope")
        .to_owned()
}

/// Pull the handle out of a `recall: <handle>` footer.
#[expect(
    clippy::string_slice,
    reason = "This slice extracts a segment from a known format where the indices are controlled."
)]
fn extract_handle(text: &str) -> String {
    let marker = "recall: ";
    let start = text.find(marker).expect("recall marker") + marker.len();
    let rest = &text[start..];
    let end = rest.find(']').expect("footer close");
    rest[..end].to_owned()
}
