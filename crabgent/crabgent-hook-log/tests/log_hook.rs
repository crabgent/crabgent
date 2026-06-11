use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, ContentBlock, Decision, Event, Hook, Kernel, LlmRequest, LlmResponse, Message,
    Notification, NotificationLevel, Outcome, RunCtx, RunId, RunRequest, StopReason, Subject, Tool,
    ToolCtx, ToolError, ToolResult, Usage,
};
use crabgent_hook_log::{LogHook, LogHookConfig, LogLevel, default_hook_chain, default_log_hook};
use crabgent_test_support::{StubProvider, done, tool_call, tool_use};
use serde_json::{Value, json};
use tracing::field::{Field, Visit};
use tracing::{Event as TracingEvent, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{Layer, Registry};

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<String>>>);

impl CapturedLogs {
    fn clear(&self) {
        self.0.lock().expect("capture lock poisoned").clear();
    }

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
        self.logs
            .push(format!("event {}", visitor.fields.join(" ")));
    }

    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::Id,
        _ctx: Context<'_, S>,
    ) {
        let mut visitor = LogVisitor::default();
        attrs.record(&mut visitor);
        self.logs.push(format!("span {}", visitor.fields.join(" ")));
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

fn global_capture() -> CapturedLogs {
    static CAPTURE: OnceLock<CapturedLogs> = OnceLock::new();
    CAPTURE
        .get_or_init(|| {
            let logs = CapturedLogs::default();
            let subscriber = Registry::default().with(CaptureLayer { logs: logs.clone() });
            if tracing::subscriber::set_global_default(subscriber).is_err() {
                // Another integration test installed the process-global subscriber first.
            }
            logs
        })
        .clone()
}

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("subject-secret"))
}

#[tokio::test]
async fn direct_hook_methods_continue_and_redact() {
    let hook = LogHook::new().with_max_field_length(12);
    let ctx = ctx();
    let call = tool_call("call-1", "lookup", json!({"token": "secret-token"}));
    let result = ToolResult::success(json!({"message": "private output"})).with_call_id("call-1");
    let req = LlmRequest {
        model: "m".into(),
        system_prompt: Some("private system".into()),
        messages: vec![json!({"role": "user", "content": "private prompt"})],
        tools: Vec::new(),
        max_tokens: Some(256),
        temperature: Some(0.2),
        stop_sequences: vec!["private stop".into()],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    };
    let resp = LlmResponse {
        text: "private llm output".into(),
        tool_calls: vec![call.clone()],
        stop_reason: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 11,
            output_tokens: 7,
            cache_creation_tokens: 3,
            cache_read_tokens: 5,
        },
        model: "m".into(),
    };
    let note = Notification {
        kind: "status".into(),
        message: "private notification".into(),
        level: NotificationLevel::Info,
    };

    let logs = CapturedLogs::default();
    let subscriber = Registry::default().with(CaptureLayer { logs: logs.clone() });
    let _guard = tracing::subscriber::set_default(subscriber);

    assert!(matches!(
        hook.on_session_start(&ctx).await,
        Decision::Continue
    ));
    assert!(matches!(
        hook.before_llm(&req, &ctx).await,
        Decision::Continue
    ));
    assert!(matches!(
        hook.after_llm(&req, &resp, &ctx).await,
        Decision::Continue
    ));
    assert!(matches!(
        hook.before_tool(&call, &ctx).await,
        Decision::Continue
    ));
    assert!(matches!(
        hook.after_tool(&call, &result, &ctx).await,
        Decision::Continue
    ));
    assert!(matches!(
        hook.on_event(&Event::Token("private text".into()), &ctx)
            .await,
        Decision::Continue
    ));
    assert!(matches!(
        hook.on_notification(&note, &ctx).await,
        Decision::Continue
    ));
    hook.on_stop(&ctx, &Outcome::Completed("private final".into()))
        .await;
    hook.on_error(
        &ctx,
        &crabgent_core::KernelError::Internal("private err".into()),
    )
    .await;

    let logs = logs.joined();

    assert!(logs.contains("kernel session started"));
    assert!(logs.contains("kernel llm request prepared"));
    assert!(logs.contains("kernel llm response observed"));
    assert!(logs.contains("kernel tool call requested"));
    assert!(logs.contains("kernel error observed"));
    assert!(logs.contains("[REDACTED len="));
    assert!(!logs.contains("private err"));
    assert!(!logs.contains("private output"));
    assert!(!logs.contains("private text"));
    assert!(!logs.contains("subject-secret"));
    assert!(!logs.contains("private system"));
    assert!(!logs.contains("private prompt"));
    assert!(!logs.contains("private stop"));
    assert!(!logs.contains("private llm output"));
    assert!(!logs.contains("secret-token"));
    assert!(!logs.contains("private notification"));
    assert!(!logs.contains("private final"));
}

#[test]
fn config_builders_set_expected_values() {
    assert_eq!(LogHookConfig::default().log_level, LogLevel::Info);
    assert_eq!(
        LogHook::default().config().max_field_length,
        LogHookConfig::DEFAULT_MAX_FIELD_LENGTH,
    );

    let config = LogHookConfig {
        log_level: LogLevel::Trace,
        max_field_length: 7,
    };
    let hook = LogHook::new()
        .with_config(config)
        .with_log_level(LogLevel::Debug)
        .with_max_field_length(9);

    assert_eq!(hook.config().log_level, LogLevel::Debug);
    assert_eq!(hook.config().max_field_length, 9);
}

#[test]
fn default_hook_helpers_keep_subscriber_setup_optional() {
    assert_eq!(
        default_log_hook().config().max_field_length,
        LogHookConfig::DEFAULT_MAX_FIELD_LENGTH,
    );

    let chain = default_hook_chain();
    assert_eq!(chain.len(), 1);
}

#[cfg(feature = "default-bundle")]
#[tokio::test(flavor = "current_thread")]
async fn install_defaults_initializes_subscriber_and_adds_log_hook() {
    let kernel = crabgent_hook_log::install_defaults(Kernel::builder())
        .provider(StubProvider::new().responses(vec![done("final-private-text")]))
        .policy(AllowAllPolicy)
        .try_build()
        .expect("kernel should build");

    assert_eq!(kernel.hooks().len(), 1);
}

#[tokio::test]
async fn trace_and_debug_paths_cover_remaining_lifecycle_surfaces() {
    let ctx = ctx();
    let note = Notification {
        kind: "notice".into(),
        message: "private notification".into(),
        level: NotificationLevel::Warn,
    };
    let call = tool_call("call-2", "lookup", json!({"value": "ä-secret-tool-args"}));
    let result = ToolResult::soft_error(json!({"err": "private output"})).with_call_id("call-2");
    let logs = CapturedLogs::default();
    let subscriber = Registry::default().with(CaptureLayer { logs: logs.clone() });
    let _guard = tracing::subscriber::set_default(subscriber);

    let trace_hook = LogHook::new()
        .with_log_level(LogLevel::Trace)
        .with_max_field_length(1);
    assert!(matches!(
        trace_hook.on_event(&Event::Notification(note), &ctx).await,
        Decision::Continue
    ));
    assert!(matches!(
        trace_hook
            .on_event(&Event::Final("private final".into()), &ctx)
            .await,
        Decision::Continue
    ));
    assert!(matches!(
        trace_hook.pre_compact(&[], &ctx).await,
        Decision::Continue
    ));
    trace_hook
        .on_stop(&ctx, &Outcome::Errored("private err".into()))
        .await;
    trace_hook.on_stop(&ctx, &Outcome::MaxTurnsExceeded).await;
    trace_hook.on_stop(&ctx, &Outcome::Cancelled).await;

    let debug_hook = LogHook::new().with_log_level(LogLevel::Debug);
    assert!(matches!(
        debug_hook.before_tool(&call, &ctx).await,
        Decision::Continue
    ));
    assert!(matches!(
        debug_hook.after_tool(&call, &result, &ctx).await,
        Decision::Continue
    ));

    let joined = logs.joined();
    assert!(joined.contains("kernel notification observed"));
    assert!(joined.contains("kernel pre-compact observed"));
    assert!(joined.contains("max_turns_exceeded"));
    assert!(!joined.contains("private notification"));
    assert!(!joined.contains("private final"));
    assert!(!joined.contains("private output"));
    assert!(!joined.contains("ä-secret-tool-args"));
}

struct NoopTool;

#[async_trait]
impl Tool for NoopTool {
    fn name(&self) -> &'static str {
        "lookup"
    }

    fn description(&self) -> &'static str {
        "test lookup"
    }

    fn parameters_schema(&self) -> Value {
        json!({})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(json!({"result": "tool-private-output"}))
    }
}

fn run_request() -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("subject-secret"),
        model: "m".into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "prompt-private-text".into(),
            }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(3),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn kernel_run_with_log_hook_reaches_tracing_subscriber() {
    let logs = global_capture();
    logs.clear();
    let kernel = Kernel::builder()
        .provider(
            StubProvider::new()
                .responses(vec![
                    tool_use(vec![tool_call(
                        "call-1",
                        "lookup",
                        json!({"query": "secret-token"}),
                    )]),
                    done("final-private-text"),
                ])
                .with_tools(true),
        )
        .policy(AllowAllPolicy)
        .add_tool(NoopTool)
        .add_hook(LogHook::new().with_log_level(LogLevel::Info))
        .try_build()
        .expect("kernel should build");

    let text = kernel.run(run_request(), None).await.expect("run succeeds");
    assert_eq!(text, "final-private-text");

    let joined = logs.joined();
    assert!(joined.contains("kernel session started"));
    assert!(joined.contains("kernel event tool call started"));
    assert!(joined.contains("kernel tool result observed"));
    assert!(joined.contains("kernel final event observed"));
    assert!(joined.contains("kernel run stopped"));
    assert!(!joined.contains("subject-secret"));
    assert!(!joined.contains("secret-token"));
    assert!(!joined.contains("tool-private-output"));
    assert!(!joined.contains("final-private-text"));
    assert!(!joined.contains("prompt-private-text"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn log_hook_emits_panic_drain_event_with_flags() {
    let logs = global_capture();
    logs.clear();

    // Provoke a real JoinError by spawning a panicking task into a
    // JoinSet, then waiting for its join_next so we get a JoinError
    // whose .is_panic() returns true.
    let mut jset = tokio::task::JoinSet::new();
    jset.spawn(async {
        panic!("test-shutdown-panic-message");
    });
    let join_err = match jset.join_next().await {
        Some(Err(e)) => e,
        other => panic!("expected panic JoinError, got {other:?}"),
    };
    assert!(join_err.is_panic());

    let hook = LogHook::new().with_log_level(LogLevel::Info);
    hook.on_kernel_shutdown_task_panic(&join_err).await;

    let joined = logs.joined();
    assert!(
        joined.contains("kernel shutdown drain observed task JoinError"),
        "missing bridge message in {joined:?}"
    );
    assert!(
        joined.contains("is_panic=true"),
        "missing is_panic flag in {joined:?}"
    );
    assert!(
        joined.contains("is_cancelled=false"),
        "missing is_cancelled flag in {joined:?}"
    );
}
