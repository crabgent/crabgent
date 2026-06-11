//! `Event::AttemptFailed` translation in `LogHook::on_event`.
//!
//! Field assertions match `Debug` of `AttemptErrorClass` (the arm formats
//! with `error_class = ?error_class`). If the enum gains a custom Display
//! or rename, these assertions need updating.

use std::sync::{Arc, Mutex};

use crabgent_core::{AttemptErrorClass, Decision, Event, Hook, RunCtx, RunId, Subject};
use crabgent_hook_log::LogHook;
use tracing::field::{Field, Visit};
use tracing::{Event as TracingEvent, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{Layer, Registry};

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<String>>>);

impl CapturedLogs {
    fn joined(&self) -> String {
        self.0.lock().expect("capture lock poisoned").join("\n")
    }

    fn push(&self, text: String) {
        self.0.lock().expect("capture lock poisoned").push(text);
    }
}

struct CaptureLayer {
    logs: CapturedLogs,
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &TracingEvent<'_>, _ctx: Context<'_, S>) {
        let mut visitor = LogVisitor::default();
        event.record(&mut visitor);
        self.logs.push(format!(
            "event level={} {}",
            event.metadata().level(),
            visitor.fields.join(" "),
        ));
    }
}

#[derive(Default)]
struct LogVisitor {
    fields: Vec<String>,
}

impl Visit for LogVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields.push(format!("{}={value:?}", field.name()));
    }
}

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("attempt-failed-subject"))
}

#[tokio::test]
async fn log_hook_emits_warn_on_terminal_attempt_failure() {
    let logs = CapturedLogs::default();
    let subscriber = Registry::default().with(CaptureLayer { logs: logs.clone() });
    let _guard = tracing::subscriber::set_default(subscriber);

    let hook = LogHook::new();
    let ctx = ctx();
    let event = Event::AttemptFailed {
        attempt_idx: 0,
        total_attempts: 2,
        provider: "anthropic".into(),
        model: "claude-haiku-4-5".into(),
        error_class: AttemptErrorClass::Auth,
        message: "auth error: bad key".into(),
        will_fallback: false,
    };

    assert!(matches!(
        hook.on_event(&event, &ctx).await,
        Decision::Continue
    ));

    let joined = logs.joined();
    assert!(
        joined.contains("level=WARN"),
        "expected WARN level, got {joined:?}"
    );
    assert!(joined.contains("kernel attempt failed (terminal)"));
    assert!(joined.contains("event=\"attempt_failed\""));
    assert!(joined.contains("attempt_idx=0"));
    assert!(joined.contains("total_attempts=2"));
    assert!(joined.contains("provider=anthropic"));
    assert!(joined.contains("model=claude-haiku-4-5"));
    assert!(joined.contains("error_class=Auth"));
    assert!(joined.contains("will_fallback=false"));
}

#[tokio::test]
async fn log_hook_emits_info_on_fallback_eligible_attempt_failure() {
    let logs = CapturedLogs::default();
    let subscriber = Registry::default().with(CaptureLayer { logs: logs.clone() });
    let _guard = tracing::subscriber::set_default(subscriber);

    let hook = LogHook::new();
    let ctx = ctx();
    let event = Event::AttemptFailed {
        attempt_idx: 0,
        total_attempts: 2,
        provider: "openai".into(),
        model: "gpt-5.5".into(),
        error_class: AttemptErrorClass::ApiServer { status: 503 },
        message: "api error: 503 busy".into(),
        will_fallback: true,
    };

    assert!(matches!(
        hook.on_event(&event, &ctx).await,
        Decision::Continue
    ));

    let joined = logs.joined();
    assert!(
        joined.contains("level=INFO"),
        "expected INFO level, got {joined:?}"
    );
    assert!(joined.contains("kernel attempt failed"));
    assert!(!joined.contains("kernel attempt failed (terminal)"));
    assert!(joined.contains("event=\"attempt_failed\""));
    assert!(joined.contains("attempt_idx=0"));
    assert!(joined.contains("provider=openai"));
    assert!(joined.contains("model=gpt-5.5"));
    assert!(joined.contains("error_class=ApiServer { status: 503 }"));
    assert!(joined.contains("will_fallback=true"));
}
