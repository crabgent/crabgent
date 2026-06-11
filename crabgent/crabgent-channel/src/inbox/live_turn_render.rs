//! Pure status-line rendering and error-classification helpers for the live turn.
//!
//! Extracted from `live_turn.rs` to keep that file under the 500-line cap. These
//! functions are stateless: they format tool/attempt status text and map kernel
//! errors to public-safe strings. They read the parent module's status-budget
//! and response-tool constants via `super::`.

use crabgent_core::error::KernelError;
use crabgent_core::hook::{AttemptErrorClass, CancelReason};
use crabgent_core::subject::Subject;
use crabgent_core::text::truncate_with_ellipsis;
use crabgent_core::types::{ToolCall, ToolResult};
use serde_json::Value;

use crate::subject::{attr_keys, parse_channel_subject_id};

use super::{DEFAULT_RESPONSE_TOOLS, MAX_ERROR_BYTES, MAX_STATUS_BYTES};

pub(super) fn current_participant_id(subject: &Subject, channel: &str) -> Option<String> {
    if let Some(participant) = subject.attr(attr_keys::PARTICIPANT_ID) {
        return Some(participant.to_owned());
    }
    parse_channel_subject_id(subject.id()).and_then(|(parsed_channel, participant)| {
        (parsed_channel == channel).then_some(participant)
    })
}

pub(super) fn render_tool_completed(call: &ToolCall, result: &ToolResult) -> String {
    let tool = display_tool_name(&call.name);
    if result.is_error {
        format!("{tool} failed: {}", tool_result_error_hint(result))
    } else {
        format!("{tool} done")
    }
}

pub(super) fn render_attempt_failed(
    provider: &str,
    model: &str,
    error_class: &AttemptErrorClass,
    message: &str,
    will_fallback: bool,
) -> String {
    let target = compact_line(&format!("{provider}/{model}"), MAX_STATUS_BYTES);
    if will_fallback {
        return format!("provider fallback: {target}");
    }
    let class = match error_class {
        AttemptErrorClass::RateLimited { .. } => "rate limited",
        AttemptErrorClass::ApiClient { .. } => "api client error",
        AttemptErrorClass::ApiServer { .. } => "api server error",
        AttemptErrorClass::Transport => "transport error",
        AttemptErrorClass::Timeout => "timeout",
        AttemptErrorClass::Auth => "auth error",
        AttemptErrorClass::MalformedResponse => "malformed response",
        AttemptErrorClass::ToolsUnsupported => "tools unsupported",
        AttemptErrorClass::VisionUnsupported => "vision unsupported",
        AttemptErrorClass::AudioUnsupported => "audio unsupported",
        AttemptErrorClass::WebSearchUnsupported => "web search unsupported",
        AttemptErrorClass::ReasoningEffortUnsupported => "reasoning effort unsupported",
        AttemptErrorClass::ModelDiscovery => "model discovery failed",
        AttemptErrorClass::Cancelled => "cancelled",
        AttemptErrorClass::RetryableStream => "stream error",
        _ => "provider error",
    };
    let detail = compact_line(message, MAX_ERROR_BYTES);
    if detail.is_empty() {
        format!("provider failed: {class}")
    } else {
        format!("provider failed: {class}: {detail}")
    }
}

pub(super) fn tool_result_error_hint(result: &ToolResult) -> String {
    compact_line(&value_text(&result.output), MAX_ERROR_BYTES)
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub(super) fn display_tool_name(name: &str) -> String {
    compact_line(name, 64)
}

pub(super) fn compact_line(input: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_bytes));
    let mut last_space = false;
    for ch in input.chars() {
        if ch.is_control() || ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
            continue;
        }
        out.push(ch);
        last_space = false;
    }
    truncate_with_ellipsis(out.trim(), max_bytes, "...").into_owned()
}

pub(super) fn is_builtin_channel_response_tool(name: &str) -> bool {
    DEFAULT_RESPONSE_TOOLS.contains(&name)
}

pub(super) const fn should_silence_error(
    err: &KernelError,
    cancel_reason: Option<CancelReason>,
    shutting_down: bool,
) -> bool {
    shutting_down
        || matches!(err, KernelError::ShuttingDown)
        || matches!(
            (err, cancel_reason),
            (KernelError::Cancelled, Some(CancelReason::StopPattern))
        )
}

pub(super) fn public_error_status(err: &KernelError) -> String {
    match err {
        KernelError::MaxTurnsExceeded(_) => "Stopped after reaching the turn limit.".to_owned(),
        KernelError::Cancelled => "Cancelled.".to_owned(),
        KernelError::Provider(_) => "Processing failed: provider error.".to_owned(),
        KernelError::Tool(_) => "Processing failed: tool error.".to_owned(),
        KernelError::HookDenied { .. } => "Processing failed: hook denied.".to_owned(),
        KernelError::PolicyDenied { .. } => "Processing failed: policy denied.".to_owned(),
        KernelError::TooManyTools { .. } => "Processing failed: too many tools.".to_owned(),
        KernelError::UnknownModel(_)
        | KernelError::AmbiguousModel(_)
        | KernelError::UnknownModelTarget(_)
        | KernelError::UnknownModelOverride { .. }
        | KernelError::ModelOverrideStore { .. }
        | KernelError::ReasoningEffortOverrideStore { .. } => {
            "Processing failed: model unavailable.".to_owned()
        }
        KernelError::Internal(_) => "Processing failed: internal error.".to_owned(),
        KernelError::ShuttingDown => String::new(),
        _ => "Processing failed.".to_owned(),
    }
}
