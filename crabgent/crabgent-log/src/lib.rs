//! Thin logging wrapper for crabgent.
//!
//! This crate is the ONLY logging interface for the crabgent workspace.
//! Use `RedactedUid` for subject identifiers and `RedactedText` for
//! message content or tool arguments.

pub use log::LevelFilter as LogLevelFilter;
pub use tracing::{
    Event, Instrument, Level, Span, Subscriber, debug, dispatcher, error, event, field, info,
    info_span, instrument, span, subscriber, trace, warn,
};

mod pii;
pub use pii::{RedactedText, RedactedUid, redact_text, redact_uid};

/// Curated default `RUST_LOG` fallback for crabgent applications.
pub const DEFAULT_DIRECTIVE: &str = "info,crabgent=debug";

/// Create a run span with the subject identifier redacted.
///
/// Pass `Subject::id()` as `subject` when calling from `crabgent-core`.
pub fn run_span(run_id: impl std::fmt::Display, subject: &str) -> Span {
    info_span!("run", run_id = %run_id, subject = %redact_uid(subject))
}

/// Create a hook span for hook lifecycle callbacks.
pub fn hook_span(name: &str) -> Span {
    info_span!("hook", name = %name)
}

/// Create a tool span keyed by tool name and run id.
pub fn tool_span(tool_name: &str, run_id: impl std::fmt::Display) -> Span {
    info_span!("tool", tool = %tool_name, run_id = %run_id)
}

/// Create a provider span keyed by provider name and model id.
pub fn provider_span(provider_name: &str, model_id: impl std::fmt::Display) -> Span {
    info_span!("provider", provider = %provider_name, model = %model_id)
}

/// Log formatter selection for the optional subscriber initializer.
#[cfg(feature = "subscriber")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable text formatter.
    Text,
    /// Structured JSON formatter.
    Json,
}

#[cfg(feature = "subscriber")]
impl LogFormat {
    /// Read `RUST_LOG_FORMAT`, using JSON only for `json`.
    pub fn from_env() -> Self {
        match std::env::var("RUST_LOG_FORMAT").as_deref() {
            Ok(value) if value.eq_ignore_ascii_case("json") => Self::Json,
            _ => Self::Text,
        }
    }
}

#[cfg(feature = "subscriber")]
static PANIC_HOOK_INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

#[cfg(feature = "subscriber")]
static LOG_TRACER_INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

#[cfg(feature = "subscriber")]
static SUBSCRIBER_INIT_ATTEMPTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

#[cfg(feature = "subscriber")]
static PII_BYPASS_WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

#[cfg(feature = "subscriber")]
fn install_panic_hook() {
    PANIC_HOOK_INSTALLED.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let location = info
                .location()
                .map_or_else(|| "<unknown>".to_owned(), ToString::to_string);
            let message = panic_payload_message(info.payload());

            error!(
                panic.location = %location,
                panic.message = %message,
                "thread panicked",
            );
            previous(info);
        }));
    });
}

#[cfg(feature = "subscriber")]
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&'static str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_owned())
}

#[cfg(feature = "subscriber")]
fn install_log_tracer() {
    LOG_TRACER_INSTALLED.get_or_init(|| {
        if let Err(error) = tracing_log::LogTracer::init() {
            info!(
                error = %error,
                "crabgent_log::init: log tracer not set",
            );
        }
    });
}

#[cfg(feature = "subscriber")]
fn warn_if_pii_bypass_active() {
    if crate::pii::pii_bypass_enabled() {
        PII_BYPASS_WARNED.get_or_init(|| {
            warn!(
                env = "CRABGENT_LOG_PII",
                "PII redaction is disabled for this debug build",
            );
        });
    }
}

#[cfg(feature = "subscriber")]
fn env_filter(default_directive: &str) -> (tracing_subscriber::EnvFilter, Option<String>) {
    match tracing_subscriber::EnvFilter::try_from_default_env() {
        Ok(filter) => (filter, None),
        Err(error) => (
            tracing_subscriber::EnvFilter::new(default_directive),
            std::env::var_os("RUST_LOG")
                .is_some()
                .then(|| error.to_string()),
        ),
    }
}

#[cfg(feature = "subscriber")]
fn init_subscriber(
    filter: tracing_subscriber::EnvFilter,
    format: LogFormat,
    should_report_error: bool,
) {
    let result = match format {
        LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .try_init(),
        LogFormat::Text => tracing_subscriber::fmt().with_env_filter(filter).try_init(),
    };

    if should_report_error && let Err(error) = result {
        info!(
            error = %error,
            "crabgent_log::init: subscriber not set",
        );
    }
}

/// Initialize the global tracing subscriber with [`DEFAULT_DIRECTIVE`].
#[cfg(feature = "subscriber")]
pub fn init_default() {
    init(DEFAULT_DIRECTIVE);
}

/// Initialize the global tracing subscriber.
///
/// Reads `RUST_LOG` from the environment and falls back to `default_directive`
/// when `RUST_LOG` is unset or invalid. `RUST_LOG_FORMAT=json` selects
/// structured JSON logs; every other value selects text logs. This also
/// installs a `log` compatibility bridge and a panic hook.
#[cfg(feature = "subscriber")]
pub fn init(default_directive: &str) {
    let should_report_subscriber_error = SUBSCRIBER_INIT_ATTEMPTED.get().is_none();
    SUBSCRIBER_INIT_ATTEMPTED.get_or_init(|| ());
    install_log_tracer();

    let (filter, rust_log_error) = env_filter(default_directive);
    init_subscriber(
        filter,
        LogFormat::from_env(),
        should_report_subscriber_error,
    );

    if let Some(error) = rust_log_error {
        warn!(
            error = %error,
            default_directive,
            "RUST_LOG parse failed, using default directive",
        );
    }
    warn_if_pii_bypass_active();
    install_panic_hook();
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
