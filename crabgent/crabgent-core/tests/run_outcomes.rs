// Shared doubles live in `lifecycle_support`; not every helper is used by this
// binary, so unused-item warnings on the shared module are expected here.
#![allow(dead_code, reason = "shared lifecycle_support has unused helpers")]

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crabgent_core::{AllowAllPolicy, Event, Kernel, KernelError, ProviderError};

mod lifecycle_support;

use lifecycle_support::{
    PendingProvider, TraceHook, done, error_provider, request, scripted, trace_snapshot,
    wait_for_trace,
};

#[tokio::test]
async fn run_provider_error_calls_on_error_then_on_stop_errored() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(error_provider())
        .policy(AllowAllPolicy)
        .add_hook(TraceHook::new(trace.clone()))
        .build();

    let err = kernel
        .run(request(5), None)
        .await
        .expect_err("provider error");

    assert!(matches!(
        err,
        KernelError::Provider(ProviderError::Other(_))
    ));
    let events = trace_snapshot(&trace);
    let on_error = events
        .iter()
        .position(|event| event.starts_with("on_error:"))
        .expect("on_error event");
    let on_stop = events
        .iter()
        .position(|event| event.starts_with("on_stop:errored:"))
        .expect("errored on_stop event");
    assert!(on_error < on_stop, "trace: {events:?}");
}

#[tokio::test]
async fn run_streaming_cancel_calls_on_stop_cancelled() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let token = CancellationToken::new();
    token.cancel();
    let kernel = Kernel::builder()
        .provider(scripted(vec![done("never")]))
        .policy(AllowAllPolicy)
        .add_hook(TraceHook::new(trace.clone()))
        .build();

    let stream = kernel.run_streaming(request(5), Some(&token));
    tokio::pin!(stream);
    let item = stream.next().await.expect("one terminal item");

    assert!(matches!(item, Err(KernelError::Cancelled)));
    assert!(trace_snapshot(&trace).contains(&"on_stop:cancelled".to_string()));
}

#[tokio::test]
async fn dropping_stream_allows_cancelled_on_stop_before_abort_watchdog() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(PendingProvider)
        .policy(AllowAllPolicy)
        .add_hook(TraceHook::new(trace.clone()))
        .build();

    let stream = Box::pin(kernel.run_streaming(request(5), None));
    wait_for_trace(&trace, "before_llm").await;
    drop(stream);

    wait_for_trace(&trace, "on_stop:cancelled").await;
}

#[tokio::test]
async fn run_and_run_streaming_share_hook_trace_for_simple_completion() {
    let sync_trace = Arc::new(Mutex::new(Vec::new()));
    let sync_kernel = Kernel::builder()
        .provider(scripted(vec![done("same")]))
        .policy(AllowAllPolicy)
        .add_hook(TraceHook::new(sync_trace.clone()))
        .build();
    let text = sync_kernel.run(request(5), None).await.expect("sync ok");
    assert_eq!(text, "same");

    let streaming_trace = Arc::new(Mutex::new(Vec::new()));
    let streaming_kernel = Kernel::builder()
        .provider(scripted(vec![done("same")]))
        .policy(AllowAllPolicy)
        .add_hook(TraceHook::new(streaming_trace.clone()))
        .build();
    let stream = streaming_kernel.run_streaming(request(5), None);
    tokio::pin!(stream);
    let mut final_text = None;
    while let Some(item) = stream.next().await {
        if let Event::Final(text) = item.expect("stream item") {
            final_text = Some(text);
            break;
        }
    }

    assert_eq!(final_text.as_deref(), Some("same"));
    assert_eq!(
        trace_snapshot(&sync_trace),
        trace_snapshot(&streaming_trace)
    );
}
