// Shared doubles live in `lifecycle_support`; not every helper is used by this
// binary, so unused-item warnings on the shared module are expected here.
#![allow(dead_code, reason = "shared lifecycle_support has unused helpers")]

use std::sync::{Arc, Mutex};

use crabgent_core::{AllowAllPolicy, Kernel, KernelError, Message};

mod lifecycle_support;

use lifecycle_support::{
    CaptureMessageHook, DenyToolResultHook, NoopTool, ReplaceAssistantHook, TraceHook,
    calling_tool, done, request, scripted, trace_snapshot,
};

#[tokio::test]
async fn run_calls_on_message_for_user_and_assistant_appends() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(scripted(vec![done("ok")]))
        .policy(AllowAllPolicy)
        .add_hook(TraceHook::new(trace.clone()))
        .build();

    kernel.run(request(5), None).await.expect("ok");

    let events = trace_snapshot(&trace);
    let on_message: Vec<_> = events
        .iter()
        .filter(|event| event.starts_with("on_message:"))
        .cloned()
        .collect();
    assert_eq!(on_message, ["on_message:1", "on_message:2"]);
}

#[tokio::test]
async fn run_on_message_replace_rewrites_canonical_message_log() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(scripted(vec![done("original")]))
        .policy(AllowAllPolicy)
        .add_hook(ReplaceAssistantHook)
        .add_hook(CaptureMessageHook::new(seen.clone()))
        .build();

    let text = kernel.run(request(5), None).await.expect("ok");
    assert_eq!(text, "original");

    let messages = seen.lock().expect("mutex should not be poisoned").clone();
    assert_eq!(messages.len(), 2);
    assert!(matches!(
        &messages[1],
        Message::Assistant { text, .. } if text == "replaced"
    ));
}

#[tokio::test]
async fn run_on_message_deny_tool_result_finishes_as_hook_error() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(scripted(vec![calling_tool(), done("unused")]))
        .policy(AllowAllPolicy)
        .add_tool(NoopTool)
        .add_hook(DenyToolResultHook)
        .add_hook(TraceHook::new(trace.clone()))
        .build();

    let err = kernel
        .run(request(5), None)
        .await
        .expect_err("tool result denial");

    assert!(matches!(
        err,
        KernelError::HookDenied { reason } if reason == "tool result denied"
    ));
    let events = trace_snapshot(&trace);
    assert!(
        events
            .iter()
            .any(|event| event.starts_with("on_stop:errored:hook denied: tool result denied")),
        "trace: {events:?}"
    );
}

#[tokio::test]
async fn run_on_stop_covers_completed_and_max_turns() {
    let completed_trace = Arc::new(Mutex::new(Vec::new()));
    let completed_kernel = Kernel::builder()
        .provider(scripted(vec![done("ok")]))
        .policy(AllowAllPolicy)
        .add_hook(TraceHook::new(completed_trace.clone()))
        .build();
    completed_kernel.run(request(5), None).await.expect("ok");
    assert!(trace_snapshot(&completed_trace).contains(&"on_stop:completed:ok".to_string()));

    let max_turns_trace = Arc::new(Mutex::new(Vec::new()));
    let max_turns_kernel = Kernel::builder()
        .provider(scripted(vec![calling_tool()]))
        .policy(AllowAllPolicy)
        .add_tool(NoopTool)
        .add_hook(TraceHook::new(max_turns_trace.clone()))
        .build();
    let err = max_turns_kernel
        .run(request(1), None)
        .await
        .expect_err("max turns");
    assert!(matches!(err, KernelError::MaxTurnsExceeded(1)));
    assert!(trace_snapshot(&max_turns_trace).contains(&"on_stop:max_turns".to_string()));
}
