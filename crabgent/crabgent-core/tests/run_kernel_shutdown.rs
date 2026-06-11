//! Integration tests for `Kernel::shutdown` graceful drain semantics.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use tokio::sync::Notify;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    AllowAllPolicy, ContentBlock, EventStream, Kernel, KernelError, LlmRequest, LlmResponse,
    Message, ModelInfo, Provider, ProviderCapabilities, ProviderError, ProviderEvent, RunCtx,
    RunId, RunRequest, Subject,
};

#[derive(Clone)]
struct HangingStreamProvider {
    state: Arc<HangingStreamState>,
}

struct HangingStreamState {
    active: AtomicUsize,
    notify: Notify,
}

impl HangingStreamProvider {
    fn new() -> Self {
        Self {
            state: Arc::new(HangingStreamState {
                active: AtomicUsize::new(0),
                notify: Notify::new(),
            }),
        }
    }

    fn active(&self) -> usize {
        self.state.active.load(Ordering::SeqCst)
    }

    async fn wait_active(&self) {
        timeout(Duration::from_secs(2), async {
            while self.active() == 0 {
                self.state.notify.notified().await;
            }
        })
        .await
        .expect("provider stream became active");
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
        "hanging-shutdown"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("test", "hanging-shutdown")]
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
        self.state.notify.notify_waiters();
    }
}

fn make_request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("shutdown-user"),
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
async fn kernel_shutdown_cancels_in_flight_streaming_run() {
    let provider = HangingStreamProvider::new();
    let kernel = Arc::new(
        Kernel::builder()
            .provider(provider.clone())
            .policy(AllowAllPolicy)
            .with_graceful_shutdown(Duration::from_secs(2))
            .build(),
    );

    let stream = kernel.run_streaming(make_request(), None);
    tokio::pin!(stream);
    provider.wait_active().await;

    let start = Instant::now();
    kernel.shutdown().await;
    let shutdown_elapsed = start.elapsed();

    let item = timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("stream yielded an item after shutdown")
        .expect("stream produced an item");
    assert!(
        matches!(item, Err(KernelError::Cancelled)),
        "expected Cancelled, got {item:?}"
    );
    assert!(
        shutdown_elapsed < Duration::from_secs(2),
        "shutdown should drain quickly via cooperative cancel, took {shutdown_elapsed:?}"
    );
    assert!(kernel.shutdown_token().is_cancelled());
    assert_eq!(provider.active(), 0);
}

#[tokio::test]
async fn kernel_shutdown_rejects_new_runs() {
    let provider = HangingStreamProvider::new();
    let kernel = Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .build();

    kernel.shutdown().await;

    let stream = kernel.run_streaming(make_request(), None);
    tokio::pin!(stream);
    let item = timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("stream yields immediately after shutdown")
        .expect("stream produced an item");
    assert!(
        matches!(item, Err(KernelError::ShuttingDown)),
        "expected ShuttingDown, got {item:?}"
    );
}

#[tokio::test]
async fn already_cancelled_caller_token_skips_provider_call() {
    let provider = HangingStreamProvider::new();
    let kernel = Kernel::builder()
        .provider(provider.clone())
        .policy(AllowAllPolicy)
        .build();

    let caller = CancellationToken::new();
    caller.cancel(); // pre-cancel
    let stream = kernel.run_streaming(make_request(), Some(&caller));
    tokio::pin!(stream);
    let item = timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("stream yields immediately")
        .expect("stream produced an item");
    assert!(matches!(item, Err(KernelError::Cancelled)));

    // Provider must never have been reached, since the caller was already
    // cancelled at run_streaming entry.
    assert_eq!(
        provider.active(),
        0,
        "provider stream was opened despite pre-cancelled caller token"
    );
}

#[tokio::test]
async fn kernel_drop_does_not_kill_in_flight_stream() {
    let provider = HangingStreamProvider::new();
    let kernel = Kernel::builder()
        .provider(provider.clone())
        .policy(AllowAllPolicy)
        .build();

    let caller = CancellationToken::new();
    let stream = kernel.run_streaming(make_request(), Some(&caller));
    tokio::pin!(stream);
    provider.wait_active().await;

    // Drop the kernel. The driver task is held alive via the
    // Arc<Mutex<JoinSet>> clone inside the stream.
    drop(kernel);

    // Cancel via caller so the driver exits cleanly.
    caller.cancel();
    let item = timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("stream yielded after caller cancel post kernel-drop")
        .expect("stream produced an item");
    assert!(matches!(item, Err(KernelError::Cancelled)));
}

#[tokio::test]
async fn kernel_shutdown_propagates_to_caller_supplied_cancel_child() {
    // When the caller supplies its own cancel token, the per-run token is a
    // child of the caller (so caller cancellation stays synchronous); the
    // kernel installs a watcher task that bridges shutdown_token -> per_run.
    // `Kernel::shutdown` therefore still propagates without the caller
    // having to do anything, and the caller-supplied token stays untouched.
    let provider = HangingStreamProvider::new();
    let kernel = Arc::new(
        Kernel::builder()
            .provider(provider.clone())
            .policy(AllowAllPolicy)
            .with_graceful_shutdown(Duration::from_secs(2))
            .build(),
    );

    let caller_token = CancellationToken::new();
    let stream = kernel.run_streaming(make_request(), Some(&caller_token));
    tokio::pin!(stream);
    provider.wait_active().await;

    kernel.shutdown().await;
    let item = timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("stream yielded after shutdown")
        .expect("stream produced an item");
    assert!(matches!(item, Err(KernelError::Cancelled)));
    assert!(!caller_token.is_cancelled());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kernel_shutdown_stress_handles_many_concurrent_spawns() {
    // GS-4 stress test: 100 concurrent run_streaming tasks on a single
    // kernel; the cooperative-cancel path under shutdown completes
    // within the grace window without leaking tasks, hanging, or
    // panicking. Uses the public API (run_streaming) so the JoinSet
    // is populated through the same code path production runs use.
    const RUN_COUNT: usize = 100;
    let provider = HangingStreamProvider::new();
    let kernel = Arc::new(
        Kernel::builder()
            .provider(provider.clone())
            .policy(AllowAllPolicy)
            .with_graceful_shutdown(Duration::from_secs(2))
            .build(),
    );

    let mut streams = Vec::with_capacity(RUN_COUNT);
    for _ in 0..RUN_COUNT {
        streams.push(kernel.run_streaming(make_request(), None));
    }
    timeout(Duration::from_secs(10), async {
        while provider.active() < RUN_COUNT {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all run_streaming tasks reached the provider");

    let start = Instant::now();
    timeout(Duration::from_secs(5), kernel.shutdown())
        .await
        .expect("shutdown completed within timeout");
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(3),
        "cooperative drain took {elapsed:?}, expected well under the 2s grace"
    );
    assert!(kernel.shutdown_token().is_cancelled());
    drop(streams);
}
