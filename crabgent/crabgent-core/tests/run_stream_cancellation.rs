use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    AllowAllPolicy, ContentBlock, EventStream, Kernel, KernelError, LlmRequest, LlmResponse,
    Message, ModelInfo, Provider, ProviderCapabilities, ProviderError, ProviderEvent, RunCtx,
    RunId, RunRequest, Subject, Tool, ToolCall, ToolCtx, ToolError,
};

#[derive(Clone)]
struct HangingStreamProvider {
    state: Arc<HangingStreamState>,
}

struct HangingStreamState {
    active: AtomicUsize,
    dropped: AtomicUsize,
    notify: Notify,
}

impl HangingStreamProvider {
    fn new() -> Self {
        Self {
            state: Arc::new(HangingStreamState {
                active: AtomicUsize::new(0),
                dropped: AtomicUsize::new(0),
                notify: Notify::new(),
            }),
        }
    }

    fn active(&self) -> usize {
        self.state.active.load(Ordering::SeqCst)
    }

    fn dropped(&self) -> usize {
        self.state.dropped.load(Ordering::SeqCst)
    }

    async fn wait_active(&self) {
        self.wait_for(|| self.active() == 1, "provider stream became active")
            .await;
    }

    async fn wait_dropped(&self) {
        self.wait_for(|| self.dropped() == 1, "provider stream was dropped")
            .await;
    }

    async fn wait_for(&self, predicate: impl Fn() -> bool + Send + Sync, msg: &'static str) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !predicate() {
                self.state.notify.notified().await;
            }
        })
        .await
        .expect(msg);
    }
}

#[async_trait]
impl Provider for HangingStreamProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Other("native stream expected".into()))
    }

    async fn stream(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<EventStream, ProviderError> {
        self.state.active.fetch_add(1, Ordering::SeqCst);
        self.state.notify.notify_waiters();
        Ok(Box::pin(HangingEventStream {
            state: Arc::clone(&self.state),
        }))
    }

    fn name(&self) -> &'static str {
        "hanging-stream"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("test", "hanging-stream")]
    }
}

struct HangingEventStream {
    state: Arc<HangingStreamState>,
}

impl Stream for HangingEventStream {
    type Item = Result<ProviderEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

impl Drop for HangingEventStream {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::SeqCst);
        self.state.dropped.fetch_add(1, Ordering::SeqCst);
        self.state.notify.notify_waiters();
    }
}

#[derive(Clone)]
struct ToolThenPendingProvider {
    state: Arc<ToolThenPendingState>,
}

struct ToolThenPendingState {
    emitted_tool: AtomicUsize,
    dropped: AtomicUsize,
    notify: Notify,
}

impl ToolThenPendingProvider {
    fn new() -> Self {
        Self {
            state: Arc::new(ToolThenPendingState {
                emitted_tool: AtomicUsize::new(0),
                dropped: AtomicUsize::new(0),
                notify: Notify::new(),
            }),
        }
    }

    async fn wait_tool_event(&self) {
        self.wait_for(
            || self.state.emitted_tool.load(Ordering::SeqCst) == 1,
            "tool event was emitted",
        )
        .await;
    }

    async fn wait_dropped(&self) {
        self.wait_for(
            || self.state.dropped.load(Ordering::SeqCst) == 1,
            "provider stream was dropped",
        )
        .await;
    }

    async fn wait_for(&self, predicate: impl Fn() -> bool + Send + Sync, msg: &'static str) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !predicate() {
                self.state.notify.notified().await;
            }
        })
        .await
        .expect(msg);
    }
}

#[async_trait]
impl Provider for ToolThenPendingProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Err(ProviderError::Other("native stream expected".into()))
    }

    async fn stream(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(ToolThenPendingStream {
            state: Arc::clone(&self.state),
            emitted: false,
        }))
    }

    fn name(&self) -> &'static str {
        "tool-then-pending"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("test", "tool-then-pending")]
    }
}

struct ToolThenPendingStream {
    state: Arc<ToolThenPendingState>,
    emitted: bool,
}

impl Stream for ToolThenPendingStream {
    type Item = Result<ProviderEvent, ProviderError>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.emitted {
            return Poll::Pending;
        }

        self.emitted = true;
        self.state.emitted_tool.fetch_add(1, Ordering::SeqCst);
        self.state.notify.notify_waiters();
        Poll::Ready(Some(Ok(ProviderEvent::ToolUse(ToolCall {
            id: "call-1".into(),
            name: "noop".into(),
            args: json!({}),
            thought_signature: None,
        }))))
    }
}

impl Drop for ToolThenPendingStream {
    fn drop(&mut self) {
        self.state.dropped.fetch_add(1, Ordering::SeqCst);
        self.state.notify.notify_waiters();
    }
}

struct CountingTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn description(&self) -> &'static str {
        "test noop"
    }

    fn parameters_schema(&self) -> Value {
        json!({})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({}))
    }
}

fn hanging_stream_kernel(provider: HangingStreamProvider) -> Kernel {
    Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .build()
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
async fn streaming_cancel_drops_pending_provider_stream() {
    let token = CancellationToken::new();
    let provider = HangingStreamProvider::new();
    let kernel = hanging_stream_kernel(provider.clone());
    let stream = kernel.run_streaming(make_request(), Some(&token));
    tokio::pin!(stream);
    provider.wait_active().await;

    token.cancel();
    let item = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("cancel produced stream item")
        .expect("one stream item");

    assert!(matches!(item, Err(KernelError::Cancelled)));
    provider.wait_dropped().await;
    assert_eq!(provider.active(), 0);
}

#[tokio::test]
async fn streaming_cancel_after_tool_use_event_skips_tool_dispatch() {
    let token = CancellationToken::new();
    let provider = ToolThenPendingProvider::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::builder()
        .provider(provider.clone())
        .policy(AllowAllPolicy)
        .add_tool(CountingTool {
            calls: calls.clone(),
        })
        .build();
    let stream = kernel.run_streaming(make_request(), Some(&token));
    tokio::pin!(stream);
    provider.wait_tool_event().await;

    token.cancel();
    let item = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("cancel produced stream item")
        .expect("one stream item");

    assert!(matches!(item, Err(KernelError::Cancelled)));
    provider.wait_dropped().await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn dropping_run_stream_aborts_pending_provider_stream() {
    let provider = HangingStreamProvider::new();
    let kernel = hanging_stream_kernel(provider.clone());
    let stream = kernel.run_streaming(make_request(), None);
    provider.wait_active().await;

    drop(stream);

    provider.wait_dropped().await;
    assert_eq!(provider.active(), 0);
}
