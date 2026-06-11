use super::*;

use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::Attributes;
use tracing::{Id, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{Layer, Registry};

#[derive(Clone, Default)]
struct CapturedFields(Arc<Mutex<Vec<(String, String)>>>);

struct CaptureLayer {
    fields: CapturedFields,
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);
        *self.fields.0.lock().expect("field capture lock poisoned") = visitor.fields;
    }
}

#[derive(Default)]
struct FieldVisitor {
    fields: Vec<(String, String)>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .push((field.name().to_owned(), format!("{value:?}")));
    }
}

fn capture_span_fields(create_span: impl FnOnce() -> Span) -> Vec<(String, String)> {
    let fields = CapturedFields::default();
    let layer = CaptureLayer {
        fields: fields.clone(),
    };
    let subscriber = Registry::default().with(layer);

    subscriber::with_default(subscriber, || {
        let _span = create_span();
    });

    fields
        .0
        .lock()
        .expect("field capture lock poisoned")
        .clone()
}

fn field_value<'a>(fields: &'a [(String, String)], name: &str) -> &'a str {
    fields
        .iter()
        .find_map(|(field, value)| (field == name).then_some(value.as_str()))
        .unwrap_or_else(|| panic!("missing span field {name} in {fields:?}"))
}

#[test]
fn redact_uid_is_deterministic() {
    let first = format!("{}", redact_uid("subject-123"));
    let second = format!("{}", redact_uid("subject-123"));

    assert_eq!(first, second);
    assert!(first.starts_with("u:"));
    assert_eq!(first.len(), 18);
}

#[test]
fn redact_uid_differs_by_input() {
    let first = format!("{}", redact_uid("subject-123"));
    let second = format!("{}", redact_uid("subject-456"));

    assert_ne!(first, second);
}

#[test]
fn redact_uid_hides_raw_subject() {
    let output = format!("{}", redact_uid("subject-123"));

    assert!(!output.contains("subject-123"));
}

#[test]
fn redact_uid_debug_matches_display() {
    let uid = redact_uid("subject-123");

    assert_eq!(format!("{uid}"), format!("{uid:?}"));
}

#[test]
fn redact_text_hides_raw_content() {
    let output = format!("{}", redact_text("message content with tool args"));

    assert!(!output.contains("message content with tool args"));
}

#[test]
fn redact_text_reports_byte_len() {
    assert_eq!(format!("{}", redact_text("tool args")), "[REDACTED len=9]");
}

#[test]
fn redact_text_debug_matches_display() {
    let text = redact_text("message content");

    assert_eq!(format!("{text}"), format!("{text:?}"));
}

#[test]
fn run_span_records_redacted_subject() {
    let raw_subject = "subject-123";
    let expected_subject = format!("{}", redact_uid(raw_subject));
    let fields = capture_span_fields(|| run_span("run-1", raw_subject));

    assert_eq!(field_value(&fields, "run_id"), "run-1");
    assert_eq!(field_value(&fields, "subject"), expected_subject);
    assert!(
        !field_value(&fields, "subject").contains(raw_subject),
        "raw subject leaked in run span: {fields:?}",
    );
}

#[test]
fn hook_span_records_name() {
    let fields = capture_span_fields(|| hook_span("audit"));

    assert_eq!(field_value(&fields, "name"), "audit");
}

#[test]
fn tool_span_records_tool_and_run() {
    let fields = capture_span_fields(|| tool_span("read_file", "run-1"));

    assert_eq!(field_value(&fields, "tool"), "read_file");
    assert_eq!(field_value(&fields, "run_id"), "run-1");
}

#[test]
fn provider_span_records_provider_and_model() {
    let fields = capture_span_fields(|| provider_span("anthropic", "claude-sonnet-4-6"));

    assert_eq!(field_value(&fields, "provider"), "anthropic");
    assert_eq!(field_value(&fields, "model"), "claude-sonnet-4-6");
}

#[cfg(debug_assertions)]
#[test]
fn debug_env_bypass_reads_env_in_child_process() {
    if std::env::var_os("CRABGENT_LOG_PII_CHILD").is_some() {
        assert_eq!(format!("{}", redact_uid("subject-123")), "subject-123");
        assert_eq!(
            format!("{}", redact_text("message content")),
            "message content"
        );
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .args([
            "--exact",
            "tests::debug_env_bypass_reads_env_in_child_process",
            "--nocapture",
        ])
        .env("CRABGENT_LOG_PII", "1")
        .env("CRABGENT_LOG_PII_CHILD", "1")
        .output()
        .expect("child test process");

    assert!(
        output.status.success(),
        "child process failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[cfg(not(debug_assertions))]
#[test]
fn release_build_ignores_bypass_env() {
    // SAFETY: this test mutates the process environment before any read in
    // this test body and removes the variable before returning.
    unsafe {
        std::env::set_var("CRABGENT_LOG_PII", "1");
    }

    let uid = format!("{}", redact_uid("subject-123"));
    let text = format!("{}", redact_text("message content"));

    // SAFETY: cleanup for the process environment mutation above.
    unsafe {
        std::env::remove_var("CRABGENT_LOG_PII");
    }

    assert!(uid.starts_with("u:"));
    assert!(!uid.contains("subject-123"));
    assert_eq!(text, "[REDACTED len=15]");
}

#[cfg(feature = "subscriber")]
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(feature = "subscriber")]
fn set_env_var(key: &str, value: &str) {
    // SAFETY: subscriber tests serialize process environment mutations through
    // ENV_LOCK before calling this helper.
    unsafe {
        std::env::set_var(key, value);
    }
}

#[cfg(feature = "subscriber")]
fn remove_env_var(key: &str) {
    // SAFETY: subscriber tests serialize process environment mutations through
    // ENV_LOCK before calling this helper.
    unsafe {
        std::env::remove_var(key);
    }
}

#[cfg(feature = "subscriber")]
#[test]
fn log_format_defaults_to_text_when_unset() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    remove_env_var("RUST_LOG_FORMAT");

    assert_eq!(LogFormat::from_env(), LogFormat::Text);
}

#[cfg(feature = "subscriber")]
#[test]
fn log_format_json_when_env_set() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    set_env_var("RUST_LOG_FORMAT", "json");
    let format = LogFormat::from_env();
    remove_env_var("RUST_LOG_FORMAT");

    assert_eq!(format, LogFormat::Json);
}

#[cfg(feature = "subscriber")]
#[test]
fn log_format_json_is_case_insensitive() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    set_env_var("RUST_LOG_FORMAT", "JSON");
    let format = LogFormat::from_env();
    remove_env_var("RUST_LOG_FORMAT");

    assert_eq!(format, LogFormat::Json);
}

#[cfg(feature = "subscriber")]
#[test]
fn log_format_unknown_value_falls_back_to_text() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    set_env_var("RUST_LOG_FORMAT", "yaml");
    let format = LogFormat::from_env();
    remove_env_var("RUST_LOG_FORMAT");

    assert_eq!(format, LogFormat::Text);
}

#[cfg(feature = "subscriber")]
#[test]
fn init_is_idempotent_across_formats() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    remove_env_var("RUST_LOG_FORMAT");
    remove_env_var("RUST_LOG");
    init("crabgent=info");
    set_env_var("RUST_LOG_FORMAT", "json");
    init("crabgent=info");
    remove_env_var("RUST_LOG_FORMAT");

    assert!(PANIC_HOOK_INSTALLED.get().is_some());
    assert!(LOG_TRACER_INSTALLED.get().is_some());
}

#[cfg(feature = "subscriber")]
#[test]
fn default_directive_is_curated_for_crabgent() {
    assert_eq!(DEFAULT_DIRECTIVE, "info,crabgent=debug");
}

#[cfg(feature = "subscriber")]
#[test]
fn init_default_uses_curated_directive() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    remove_env_var("RUST_LOG_FORMAT");
    remove_env_var("RUST_LOG");
    init_default();

    assert!(PANIC_HOOK_INSTALLED.get().is_some());
}

#[cfg(feature = "subscriber")]
#[test]
fn env_filter_accepts_valid_rust_log() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    set_env_var("RUST_LOG", "crabgent_log=debug");
    let (_filter, error) = env_filter("info");
    remove_env_var("RUST_LOG");

    assert!(error.is_none());
}

#[cfg(feature = "subscriber")]
#[test]
fn init_warns_after_invalid_rust_log_fallback() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    set_env_var("RUST_LOG", "[");
    remove_env_var("RUST_LOG_FORMAT");
    init("info");
    remove_env_var("RUST_LOG");

    assert!(PANIC_HOOK_INSTALLED.get().is_some());
}

#[cfg(feature = "subscriber")]
#[test]
fn panic_payload_message_handles_common_payloads() {
    assert_eq!(panic_payload_message(&"literal panic"), "literal panic");
    assert_eq!(
        panic_payload_message(&String::from("owned panic")),
        "owned panic",
    );
    assert_eq!(panic_payload_message(&7_u8), "<non-string panic payload>");
}

#[cfg(feature = "subscriber")]
#[test]
fn panic_hook_logs_and_delegates() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    install_panic_hook();
    let result = std::panic::catch_unwind(|| panic!("subscriber hook coverage"));

    assert!(result.is_err());
}
