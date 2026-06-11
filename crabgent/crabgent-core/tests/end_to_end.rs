//! End-to-end integration test: builds a kernel with all four builtin
//! tools and exercises the public API surface from outside the crate.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    AllowAllPolicy, BashTool, ContentBlock, Decision, Event, Hook, Kernel, KernelError, LlmRequest,
    LlmResponse, Message, ModelInfo, ReadFileTool, RunCtx, RunId, RunRequest, Subject,
    WriteFileTool,
};
use crabgent_test_support::{StubProvider, done, tool_call, tool_use};

#[path = "common/noop_tool.rs"]
mod noop_tool;

use noop_tool::NoopTool;

fn scripted(responses: Vec<LlmResponse>) -> StubProvider {
    StubProvider::new()
        .responses(responses)
        .with_tools(true)
        .with_models(vec![
            ModelInfo::minimal("m", "stub"),
            ModelInfo::minimal("test", "stub"),
        ])
}

fn calling_tool(name: &str, id: &str, args: Value) -> LlmResponse {
    tool_use(vec![tool_call(id, name, args)])
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
                text: "hello".into(),
            }],
            timestamp: None,
        }],
        system_prompt: Some("you are testing".into()),
        max_turns: Some(5),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

#[tokio::test]
async fn run_with_no_tools_returns_text() {
    let kernel = Kernel::builder()
        .provider(scripted(vec![done("hi back")]))
        .policy(AllowAllPolicy)
        .build();
    let text = kernel.run(make_request(), None).await.expect("ok");
    assert_eq!(text, "hi back");
}

#[tokio::test]
async fn run_dispatches_read_file_builtin() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("greeting.txt");
    std::fs::write(&path, "hello world").expect("write");

    let kernel = Kernel::builder()
        .provider(scripted(vec![
            calling_tool(
                "read_file",
                "c1",
                json!({"path": path.to_str().expect("path should be valid UTF-8")}),
            ),
            done("read it"),
        ]))
        .policy(AllowAllPolicy)
        .add_tool(ReadFileTool::without_root())
        .build();
    let text = kernel.run(make_request(), None).await.expect("ok");
    assert_eq!(text, "read it");
}

#[tokio::test]
async fn run_dispatches_write_file_builtin() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("note.txt");
    let path_str = path
        .to_str()
        .expect("path should be valid UTF-8")
        .to_string();

    let kernel = Kernel::builder()
        .provider(scripted(vec![
            calling_tool(
                "write_file",
                "c1",
                json!({"path": path_str, "content": "from kernel"}),
            ),
            done("wrote"),
        ]))
        .policy(AllowAllPolicy)
        .add_tool(WriteFileTool::without_root())
        .build();
    kernel.run(make_request(), None).await.expect("ok");
    assert_eq!(
        std::fs::read_to_string(&path).expect("test result"),
        "from kernel"
    );
}

#[tokio::test]
async fn run_dispatches_bash_builtin() {
    let kernel = Kernel::builder()
        .provider(scripted(vec![
            calling_tool("bash", "c1", json!({"command": "echo hi"})),
            done("bash ran"),
        ]))
        .policy(AllowAllPolicy)
        .add_tool(BashTool::new())
        .build();
    let text = kernel.run(make_request(), None).await.expect("ok");
    assert_eq!(text, "bash ran");
}

struct CountingHook(AtomicUsize);

#[async_trait]
impl Hook for CountingHook {
    async fn before_llm(&self, _req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Decision::Continue
    }
}

struct CountingHookArc(std::sync::Arc<CountingHook>);

#[async_trait]
impl Hook for CountingHookArc {
    async fn before_llm(&self, req: &LlmRequest, ctx: &RunCtx) -> Decision<LlmRequest> {
        self.0.before_llm(req, ctx).await
    }
}

#[tokio::test]
async fn hook_observes_each_iteration() {
    let counter = std::sync::Arc::new(CountingHook(AtomicUsize::new(0)));
    let kernel = Kernel::builder()
        .provider(scripted(vec![
            calling_tool("noop", "c1", json!({})),
            done("done"),
        ]))
        .policy(AllowAllPolicy)
        .add_tool(NoopTool)
        .add_hook(CountingHookArc(counter.clone()))
        .build();
    kernel.run(make_request(), None).await.expect("ok");
    assert_eq!(counter.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cancel_token_aborts_run_before_first_call() {
    let token = CancellationToken::new();
    token.cancel();
    let kernel = Kernel::builder()
        .provider(scripted(vec![done("never")]))
        .policy(AllowAllPolicy)
        .build();
    let r = kernel.run(make_request(), Some(&token)).await;
    assert!(matches!(r, Err(KernelError::Cancelled)));
}

#[tokio::test]
async fn streaming_no_tools_emits_token_then_final() {
    let kernel = Kernel::builder()
        .provider(scripted(vec![done("hello world")]))
        .policy(AllowAllPolicy)
        .build();
    let stream = kernel.run_streaming(make_request(), None);
    tokio::pin!(stream);
    let mut tokens = Vec::new();
    let mut final_text = None;
    while let Some(item) = stream.next().await {
        match item.expect("ok event") {
            Event::Token(s) => tokens.push(s),
            Event::Final(s) => {
                final_text = Some(s);
                break;
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(tokens.concat(), "hello world");
    assert_eq!(final_text.as_deref(), Some("hello world"));
}

#[tokio::test]
async fn streaming_emits_tool_lifecycle_events() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("greeting.txt");
    std::fs::write(&path, "hi").expect("write");

    let kernel = Kernel::builder()
        .provider(scripted(vec![
            calling_tool(
                "read_file",
                "c1",
                json!({"path": path.to_str().expect("path should be valid UTF-8")}),
            ),
            done("ok"),
        ]))
        .policy(AllowAllPolicy)
        .add_tool(ReadFileTool::without_root())
        .build();

    let stream = kernel.run_streaming(make_request(), None);
    tokio::pin!(stream);
    let mut started = 0u32;
    let mut completed = 0u32;
    let mut final_seen = false;
    while let Some(item) = stream.next().await {
        match item.expect("ok event") {
            Event::ToolCallStarted(_) => started += 1,
            Event::ToolCallCompleted { .. } => completed += 1,
            Event::Final(_) => {
                final_seen = true;
                break;
            }
            _ => {}
        }
    }
    assert_eq!(started, 1);
    assert_eq!(completed, 1);
    assert!(final_seen);
}

#[tokio::test]
async fn streaming_propagates_cancel_as_error() {
    let token = CancellationToken::new();
    token.cancel();
    let kernel = Kernel::builder()
        .provider(scripted(vec![done("never")]))
        .policy(AllowAllPolicy)
        .build();
    let stream = kernel.run_streaming(make_request(), Some(&token));
    tokio::pin!(stream);
    let item = stream.next().await.expect("one item");
    assert!(matches!(item, Err(KernelError::Cancelled)));
}

struct EventCounter(AtomicUsize);

#[async_trait]
impl Hook for EventCounter {
    async fn on_event(&self, _ev: &Event, _ctx: &RunCtx) -> Decision<Event> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Decision::Continue
    }
}

struct EventCounterArc(std::sync::Arc<EventCounter>);

#[async_trait]
impl Hook for EventCounterArc {
    async fn on_event(&self, ev: &Event, ctx: &RunCtx) -> Decision<Event> {
        self.0.on_event(ev, ctx).await
    }
}

#[tokio::test]
async fn streaming_runs_on_event_hook_per_event() {
    let counter = std::sync::Arc::new(EventCounter(AtomicUsize::new(0)));
    let kernel = Kernel::builder()
        .provider(scripted(vec![done("hi")]))
        .policy(AllowAllPolicy)
        .add_hook(EventCounterArc(counter.clone()))
        .build();
    let stream = kernel.run_streaming(make_request(), None);
    tokio::pin!(stream);
    while let Some(item) = stream.next().await {
        if matches!(item, Ok(Event::Final(_))) {
            break;
        }
    }
    // One Token + one Final = 2 on_event invocations
    assert_eq!(counter.0.load(Ordering::SeqCst), 2);
}
