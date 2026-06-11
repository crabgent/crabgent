use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crabgent_core::{
    AllowAllPolicy, ContentBlock, Decision, Event, Hook, Kernel, KernelError, LlmResponse, Message,
    ModelInfo, Notification, NotificationLevel, Outcome, RunCtx, RunId, RunRequest, Subject,
};
use crabgent_test_support::{StubProvider, done_for_model};

fn scripted(responses: Vec<LlmResponse>) -> StubProvider {
    StubProvider::new()
        .responses(responses)
        .with_models(vec![ModelInfo::minimal("test", "stub")])
}

struct NotificationInjectHook;

#[async_trait]
impl Hook for NotificationInjectHook {
    async fn on_event(&self, event: &Event, _ctx: &RunCtx) -> Decision<Event> {
        if matches!(event, Event::Final(_)) {
            return Decision::Replace(Event::Notification(Notification {
                kind: "test".into(),
                message: "test notification".into(),
                level: NotificationLevel::Info,
            }));
        }
        Decision::Continue
    }
}

struct NotificationDenyHook;

#[async_trait]
impl Hook for NotificationDenyHook {
    async fn on_notification(&self, _note: &Notification, _ctx: &RunCtx) -> Decision<()> {
        Decision::Deny("test deny".into())
    }
}

struct OutcomeRecorderHook {
    events: Arc<Mutex<Vec<String>>>,
}

impl OutcomeRecorderHook {
    const fn new(events: Arc<Mutex<Vec<String>>>) -> Self {
        Self { events }
    }

    async fn push(&self, event: impl Into<String>) {
        self.events.lock().await.push(event.into());
    }
}

#[async_trait]
impl Hook for OutcomeRecorderHook {
    async fn on_stop(&self, _ctx: &RunCtx, outcome: &Outcome) {
        match outcome {
            Outcome::Completed(text) => self.push(format!("stop:completed:{text}")).await,
            Outcome::Errored(reason) => self.push(format!("stop:errored:{reason}")).await,
            Outcome::MaxTurnsExceeded => self.push("stop:max_turns").await,
            Outcome::Cancelled => self.push("stop:cancelled").await,
            _ => self.push("stop:unknown").await,
        }
    }

    async fn on_error(&self, _ctx: &RunCtx, err: &KernelError) {
        self.push(format!("error:{err}")).await;
    }
}

fn make_request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("notification-test-user"),
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
        system_prompt: None,
        max_turns: Some(5),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

#[tokio::test]
async fn notification_event_runs_notification_hook_and_propagates_deny() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let kernel = Kernel::builder()
        .provider(scripted(vec![done_for_model("hi", "test")]))
        .policy(AllowAllPolicy)
        .add_hook(NotificationInjectHook)
        .add_hook(NotificationDenyHook)
        .add_hook(OutcomeRecorderHook::new(Arc::clone(&events)))
        .build();

    let err = kernel
        .run(make_request(), None)
        .await
        .expect_err("notification deny should stop run");

    assert!(matches!(
        err,
        KernelError::HookDenied { reason } if reason == "test deny"
    ));
    let events = events.lock().await;
    assert_eq!(
        events.as_slice(),
        ["stop:completed:hi"],
        "completion notification denial should not mark run as errored"
    );
}

#[tokio::test(start_paused = true)]
async fn final_event_replace_with_continue_returns_internal_after_stream_end() {
    let kernel = Kernel::builder()
        .provider(scripted(vec![done_for_model("hi", "test")]))
        .policy(AllowAllPolicy)
        .add_hook(NotificationInjectHook)
        .build();

    let result = tokio::time::timeout(Duration::from_secs(1), kernel.run(make_request(), None))
        .await
        .expect("run did not return after the replaced final event closed the stream");
    let err = result.expect_err("replacing final event should leave run without final text");

    assert!(matches!(
        err,
        KernelError::Internal(reason)
            if reason == "run stream ended without final event"
    ));
}
