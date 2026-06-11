use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_core::{
    ContentBlock, Decision, Hook, HookChain, KernelError, LlmRequest, LlmResponse, Message,
    ModelId, Notification, NotificationLevel, Outcome, RunCtx, RunId, StopReason, Subject, Usage,
};
use serde_json::json;
use tokio::task::JoinError;
use tokio_util::sync::CancellationToken;

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("u"))
}

fn req() -> LlmRequest {
    LlmRequest {
        model: ModelId::new("test"),
        system_prompt: None,
        messages: vec![json!({"role": "user", "content": "hi"})],
        tools: vec![],
        max_tokens: None,
        temperature: None,
        stop_sequences: vec![],
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
        tool_choice: None,
    }
}

fn resp() -> LlmResponse {
    LlmResponse {
        text: "hi".into(),
        tool_calls: vec![],
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        model: ModelId::new("test"),
    }
}

struct CountingHook(Arc<AtomicUsize>);

#[async_trait]
impl Hook for CountingHook {
    async fn on_session_start(&self, _ctx: &RunCtx) -> Decision<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Decision::Continue
    }
}

struct AppendModelHook;

#[async_trait]
impl Hook for AppendModelHook {
    async fn before_llm(&self, req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        let mut next = req.clone();
        next.model = ModelId::new(format!("{}-mod", next.model.as_str()));
        Decision::Replace(next)
    }
}

struct DenyHook(String);

#[async_trait]
impl Hook for DenyHook {
    async fn before_llm(&self, _req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        Decision::Deny(self.0.clone())
    }
}

struct ContinueHook(Arc<AtomicUsize>);

#[async_trait]
impl Hook for ContinueHook {
    async fn before_llm(&self, _req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Decision::Continue
    }
}

struct CompactHook;

#[async_trait]
impl Hook for CompactHook {
    async fn pre_compact(&self, _msgs: &[Message], _ctx: &RunCtx) -> Decision<Vec<Message>> {
        Decision::Replace(vec![Message::System {
            content: "compact".into(),
        }])
    }
}

struct ForbiddenBeforeLlmHook(Arc<AtomicUsize>);

#[async_trait]
impl Hook for ForbiddenBeforeLlmHook {
    async fn before_llm(&self, _req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Decision::Continue
    }
}

struct StopCounter(Arc<AtomicUsize>);

#[async_trait]
impl Hook for StopCounter {
    async fn on_stop(&self, _ctx: &RunCtx, _outcome: &Outcome) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct ErrorCounter(Arc<AtomicUsize>);

#[async_trait]
impl Hook for ErrorCounter {
    async fn on_error(&self, _ctx: &RunCtx, _err: &KernelError) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct ShutdownCounter(Arc<AtomicUsize>);

#[async_trait]
impl Hook for ShutdownCounter {
    async fn on_kernel_shutdown(&self, _token: &CancellationToken) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct PanicCounter(Arc<AtomicUsize>);

#[async_trait]
impl Hook for PanicCounter {
    async fn on_kernel_shutdown_task_panic(&self, _err: &JoinError) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

async fn join_error() -> JoinError {
    tokio::spawn(async { panic!("intentional test panic") })
        .await
        .expect_err("spawned task should report a panic")
}

struct NotificationContinueHook;

#[async_trait]
impl Hook for NotificationContinueHook {
    async fn on_notification(&self, _note: &Notification, _ctx: &RunCtx) -> Decision<()> {
        Decision::Continue
    }
}

struct NotificationDenyHook;

#[async_trait]
impl Hook for NotificationDenyHook {
    async fn on_notification(&self, _note: &Notification, _ctx: &RunCtx) -> Decision<()> {
        Decision::Deny("notification denied".into())
    }
}

fn notification() -> Notification {
    Notification {
        kind: "test".into(),
        message: "test notification".into(),
        level: NotificationLevel::Info,
    }
}

#[tokio::test]
async fn empty_chain_passes_state_through() {
    let chain = HookChain::new();
    let out = chain.apply_before_llm(&req(), &ctx()).await.expect("ok");
    assert_eq!(out, req());
}

#[tokio::test]
async fn replace_propagates_through_chain() {
    let mut chain = HookChain::new();
    chain.push(AppendModelHook);
    chain.push(AppendModelHook);
    let out = chain.apply_before_llm(&req(), &ctx()).await.expect("ok");
    assert_eq!(out.model.as_str(), "test-mod-mod");
}

#[tokio::test]
async fn continue_replace_continue_preserves_replacement() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut chain = HookChain::new();
    chain.push(ContinueHook(counter.clone()));
    chain.push(AppendModelHook);
    chain.push(ContinueHook(counter.clone()));
    let out = chain.apply_before_llm(&req(), &ctx()).await.expect("ok");
    assert_eq!(out.model.as_str(), "test-mod");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn deny_short_circuits() {
    let forbidden_calls = Arc::new(AtomicUsize::new(0));
    let mut chain = HookChain::new();
    chain.push(AppendModelHook);
    chain.push(DenyHook("policy".into()));
    chain.push(ForbiddenBeforeLlmHook(forbidden_calls.clone()));
    let err = chain
        .apply_before_llm(&req(), &ctx())
        .await
        .expect_err("deny should return HookDenied");
    assert!(matches!(err, KernelError::HookDenied { reason } if reason == "policy"));
    assert_eq!(forbidden_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn session_start_runs_each_hook() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut chain = HookChain::new();
    chain.push(CountingHook(counter.clone()));
    chain.push(CountingHook(counter.clone()));
    chain.apply_on_session_start(&ctx()).await.expect("ok");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn on_stop_runs_even_on_errored_outcome() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut chain = HookChain::new();
    chain.push(StopCounter(counter.clone()));
    chain.push(StopCounter(counter.clone()));
    chain
        .apply_on_stop(&ctx(), &Outcome::Errored("boom".into()))
        .await;
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn on_error_runs_each_hook_without_replacing_error() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut chain = HookChain::new();
    chain.push(ErrorCounter(counter.clone()));
    chain.push(ErrorCounter(counter.clone()));
    chain
        .apply_on_error(&ctx(), &KernelError::Internal("orig".into()))
        .await;
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn on_notification_continue_returns_ok() {
    let mut chain = HookChain::new();
    chain.push(NotificationContinueHook);

    chain
        .apply_on_notification(&notification(), &ctx())
        .await
        .expect("continue should pass");
}

#[tokio::test]
async fn on_notification_deny_returns_hook_denied() {
    let mut chain = HookChain::new();
    chain.push(NotificationDenyHook);

    let err = chain
        .apply_on_notification(&notification(), &ctx())
        .await
        .expect_err("deny should fail");

    assert!(matches!(
        err,
        KernelError::HookDenied { reason } if reason == "notification denied"
    ));
}

#[tokio::test]
async fn pre_compact_returns_replacement_only_when_hook_replaces() {
    let mut chain = HookChain::new();
    chain.push(ContinueHook(Arc::new(AtomicUsize::new(0))));
    chain.push(CompactHook);
    let original = vec![Message::User {
        content: vec![ContentBlock::Text {
            text: "full".into(),
        }],
        timestamp: None,
    }];

    let out = chain
        .apply_pre_compact(&original, &ctx())
        .await
        .expect("ok")
        .expect("replacement");

    assert_eq!(out.len(), 1);
    assert!(matches!(
        out.first()
            .expect("replacement should contain first message"),
        Message::System { .. }
    ));
}

#[tokio::test]
async fn after_llm_passes_request_to_each_hook() {
    struct Capture(Arc<std::sync::Mutex<Vec<String>>>);

    #[async_trait]
    impl Hook for Capture {
        async fn after_llm(
            &self,
            req: &LlmRequest,
            _resp: &LlmResponse,
            _ctx: &RunCtx,
        ) -> Decision<LlmResponse> {
            self.0
                .lock()
                .expect("mutex should not be poisoned")
                .push(req.model.as_str().to_owned());
            Decision::Continue
        }
    }

    let captured = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let mut chain = HookChain::new();
    chain.push(Capture(captured.clone()));
    chain.push(Capture(captured.clone()));
    chain
        .apply_after_llm(&req(), &resp(), &ctx())
        .await
        .expect("ok");
    let saw = captured.lock().expect("mutex should not be poisoned");
    assert_eq!(saw.len(), 2);
    assert!(saw.iter().all(|s| s == "test"));
}

#[tokio::test]
async fn chain_len_and_is_empty() {
    let mut chain = HookChain::new();
    assert!(chain.is_empty());
    chain.push(AppendModelHook);
    assert_eq!(chain.len(), 1);
    chain.push_arc(Arc::new(AppendModelHook));
    assert_eq!(chain.len(), 2);
}

#[tokio::test]
async fn on_kernel_shutdown_runs_each_hook() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut chain = HookChain::new();
    chain.push(ShutdownCounter(counter.clone()));
    chain.push(ShutdownCounter(counter.clone()));
    chain
        .apply_on_kernel_shutdown(&CancellationToken::new())
        .await;
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn on_kernel_shutdown_task_panic_runs_each_hook() {
    let err = join_error().await;
    let counter = Arc::new(AtomicUsize::new(0));
    let mut chain = HookChain::new();
    chain.push(PanicCounter(counter.clone()));
    chain.push(PanicCounter(counter.clone()));
    chain.apply_on_kernel_shutdown_task_panic(&err).await;
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn terminal_callbacks_are_noop_on_empty_chain() {
    let chain = HookChain::new();
    chain
        .apply_on_stop(&ctx(), &Outcome::Errored("x".into()))
        .await;
    chain
        .apply_on_error(&ctx(), &KernelError::Internal("x".into()))
        .await;
    chain
        .apply_on_kernel_shutdown(&CancellationToken::new())
        .await;
    let err = join_error().await;
    chain.apply_on_kernel_shutdown_task_panic(&err).await;
    // Every terminal callback completes without panicking on an empty chain.
}
