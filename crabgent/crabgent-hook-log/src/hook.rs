use async_trait::async_trait;
use crabgent_core::{
    Decision, Event, Hook, KernelError, LlmRequest, LlmResponse, Message, Notification, Outcome,
    RunCtx, ToolCall, ToolResult,
};
use crabgent_log::{debug, error, hook_span, info, redact_text, run_span, tool_span, trace, warn};
use serde_json::Value;
use tokio::task::JoinError;

macro_rules! event_at_level {
    ($level:expr, $($field:tt)*) => {
        event_at_config_level(
            $level,
            || trace!($($field)*),
            || debug!($($field)*),
            || info!($($field)*),
        )
    };
}

fn event_at_config_level(
    level: LogLevel,
    trace_fn: impl FnOnce(),
    debug_fn: impl FnOnce(),
    info_fn: impl FnOnce(),
) {
    match level {
        LogLevel::Trace => trace_fn(),
        LogLevel::Debug => debug_fn(),
        LogLevel::Info => info_fn(),
    }
}

/// Configurable event level for [`LogHook`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
}

/// Runtime configuration for [`LogHook`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogHookConfig {
    pub log_level: LogLevel,
    pub max_field_length: usize,
}

impl LogHookConfig {
    pub const DEFAULT_MAX_FIELD_LENGTH: usize = 4096;
}

impl Default for LogHookConfig {
    fn default() -> Self {
        Self {
            log_level: LogLevel::Info,
            max_field_length: Self::DEFAULT_MAX_FIELD_LENGTH,
        }
    }
}

/// Read-only hook that forwards kernel lifecycle events to tracing.
pub struct LogHook {
    config: LogHookConfig,
}

impl LogHook {
    pub const fn new() -> Self {
        Self {
            config: LogHookConfig {
                log_level: LogLevel::Info,
                max_field_length: LogHookConfig::DEFAULT_MAX_FIELD_LENGTH,
            },
        }
    }

    pub const fn with_config(mut self, config: LogHookConfig) -> Self {
        self.config = config;
        self
    }

    pub const fn with_log_level(mut self, log_level: LogLevel) -> Self {
        self.config.log_level = log_level;
        self
    }

    pub const fn with_max_field_length(mut self, max_field_length: usize) -> Self {
        self.config.max_field_length = max_field_length;
        self
    }

    pub const fn config(&self) -> LogHookConfig {
        self.config
    }

    fn capped_text(&self, text: &str) -> String {
        let max = self.config.max_field_length;
        if text.len() <= max {
            return text.to_owned();
        }

        crabgent_core::text::truncate_bytes_at_boundary(text, max).to_owned()
    }

    fn capped_json(&self, value: &Value) -> String {
        self.capped_text(&value.to_string())
    }

    fn enter_run(ctx: &RunCtx) -> crabgent_log::Span {
        run_span(&ctx.run_id, ctx.subject.id())
    }

    fn log_notification(&self, note: &Notification, surface: &str) {
        let message = self.capped_text(&note.message);
        event_at_level!(
            self.config.log_level,
            surface,
            kind = %note.kind,
            level = ?note.level,
            message = %redact_text(&message),
            "kernel notification observed",
        );
    }

    fn log_tool_call(&self, call: &ToolCall, message: &'static str) {
        let args = self.capped_json(&call.args);
        event_at_level!(
            self.config.log_level,
            call_id = %call.id,
            tool = %call.name,
            args = %redact_text(&args),
            message,
        );
    }

    fn log_tool_result(&self, result: &ToolResult, message: &'static str) {
        let output = self.capped_json(&result.output);
        event_at_level!(
            self.config.log_level,
            call_id = %result.call_id,
            is_error = result.is_error,
            output = %redact_text(&output),
            message,
        );
    }

    fn log_llm_request(&self, req: &LlmRequest) {
        event_at_level!(
            self.config.log_level,
            model = %req.model,
            message_count = req.messages.len(),
            tool_count = req.tools.len(),
            has_system_prompt = req.system_prompt.is_some(),
            max_tokens = ?req.max_tokens,
            temperature = ?req.temperature,
            stop_sequence_count = req.stop_sequences.len(),
            "kernel llm request prepared",
        );
    }

    fn log_llm_response(&self, resp: &LlmResponse) {
        let text = self.capped_text(&resp.text);
        event_at_level!(
            self.config.log_level,
            model = %resp.model,
            stop_reason = ?resp.stop_reason,
            input_tokens = resp.usage.input_tokens,
            output_tokens = resp.usage.output_tokens,
            cache_creation_tokens = resp.usage.cache_creation_tokens,
            cache_read_tokens = resp.usage.cache_read_tokens,
            tool_call_count = resp.tool_calls.len(),
            text = %redact_text(&text),
            "kernel llm response observed",
        );
    }

    fn log_outcome(&self, outcome: &Outcome) {
        match outcome {
            Outcome::Completed(text) => {
                let text = self.capped_text(text);
                event_at_level!(
                    self.config.log_level,
                    outcome = "completed",
                    text = %redact_text(&text),
                    "kernel run stopped",
                );
            }
            Outcome::Errored(reason) => {
                let reason = self.capped_text(reason);
                event_at_level!(
                    self.config.log_level,
                    outcome = "errored",
                    reason = %redact_text(&reason),
                    "kernel run stopped",
                );
            }
            Outcome::MaxTurnsExceeded => {
                event_at_level!(
                    self.config.log_level,
                    outcome = "max_turns_exceeded",
                    "kernel run stopped",
                );
            }
            Outcome::Cancelled => {
                event_at_level!(
                    self.config.log_level,
                    outcome = "cancelled",
                    "kernel run stopped",
                );
            }
            Outcome::Paused => {
                event_at_level!(
                    self.config.log_level,
                    outcome = "paused",
                    "kernel run stopped",
                );
            }
            _ => {
                event_at_level!(
                    self.config.log_level,
                    outcome = "unknown",
                    "kernel run stopped",
                );
            }
        }
    }
}

impl Default for LogHook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Hook for LogHook {
    async fn on_session_start(&self, ctx: &RunCtx) -> Decision<()> {
        let span = Self::enter_run(ctx);
        let _guard = span.enter();
        event_at_level!(self.config.log_level, "kernel session started");
        Decision::Continue
    }

    async fn before_llm(&self, req: &LlmRequest, ctx: &RunCtx) -> Decision<LlmRequest> {
        let span = Self::enter_run(ctx);
        let _guard = span.enter();
        self.log_llm_request(req);
        Decision::Continue
    }

    async fn after_llm(
        &self,
        _req: &LlmRequest,
        resp: &LlmResponse,
        ctx: &RunCtx,
    ) -> Decision<LlmResponse> {
        let span = Self::enter_run(ctx);
        let _guard = span.enter();
        self.log_llm_response(resp);
        Decision::Continue
    }

    async fn before_tool(&self, call: &ToolCall, ctx: &RunCtx) -> Decision<ToolCall> {
        let span = tool_span(&call.name, &ctx.run_id);
        let _guard = span.enter();
        self.log_tool_call(call, "kernel tool call requested");
        Decision::Continue
    }

    async fn after_tool(
        &self,
        call: &ToolCall,
        result: &ToolResult,
        ctx: &RunCtx,
    ) -> Decision<ToolResult> {
        let span = tool_span(&call.name, &ctx.run_id);
        let _guard = span.enter();
        self.log_tool_result(result, "kernel tool result observed");
        Decision::Continue
    }

    async fn on_event(&self, ev: &Event, ctx: &RunCtx) -> Decision<Event> {
        match ev {
            Event::Token(text) => {
                let span = Self::enter_run(ctx);
                let _guard = span.enter();
                let text = self.capped_text(text);
                event_at_level!(
                    self.config.log_level,
                    event = "token",
                    text = %redact_text(&text),
                    "kernel event observed",
                );
            }
            Event::ToolCallStarted(call) => {
                let span = tool_span(&call.name, &ctx.run_id);
                let _guard = span.enter();
                self.log_tool_call(call, "kernel event tool call started");
            }
            Event::ToolCallCompleted { call, result } => {
                let span = tool_span(&call.name, &ctx.run_id);
                let _guard = span.enter();
                self.log_tool_result(result, "kernel event tool call completed");
            }
            Event::Notification(note) => {
                let span = Self::enter_run(ctx);
                let _guard = span.enter();
                self.log_notification(note, "on_event");
            }
            Event::Final(text) => {
                let span = Self::enter_run(ctx);
                let _guard = span.enter();
                let text = self.capped_text(text);
                event_at_level!(
                    self.config.log_level,
                    event = "final",
                    text = %redact_text(&text),
                    "kernel final event observed",
                );
            }
            Event::AttemptFailed {
                attempt_idx,
                total_attempts,
                provider,
                model,
                error_class,
                message,
                will_fallback,
            } => {
                let span = Self::enter_run(ctx);
                let _guard = span.enter();
                let message = self.capped_text(message);
                if *will_fallback {
                    info!(
                        event = "attempt_failed",
                        attempt_idx = *attempt_idx,
                        total_attempts = *total_attempts,
                        provider = %provider,
                        model = %model,
                        error_class = ?error_class,
                        message = %redact_text(&message),
                        will_fallback = *will_fallback,
                        "kernel attempt failed",
                    );
                } else {
                    warn!(
                        event = "attempt_failed",
                        attempt_idx = *attempt_idx,
                        total_attempts = *total_attempts,
                        provider = %provider,
                        model = %model,
                        error_class = ?error_class,
                        message = %redact_text(&message),
                        will_fallback = *will_fallback,
                        "kernel attempt failed (terminal)",
                    );
                }
            }
            _ => {}
        }
        Decision::Continue
    }

    async fn pre_compact(&self, msgs: &[Message], ctx: &RunCtx) -> Decision<Vec<Message>> {
        let span = hook_span("pre_compact");
        let _guard = span.enter();
        event_at_level!(
            self.config.log_level,
            run_id = %ctx.run_id,
            message_count = msgs.len(),
            "kernel pre-compact observed",
        );
        Decision::Continue
    }

    async fn on_stop(&self, ctx: &RunCtx, outcome: &Outcome) {
        let span = Self::enter_run(ctx);
        let _guard = span.enter();
        self.log_outcome(outcome);
    }

    async fn on_notification(&self, note: &Notification, ctx: &RunCtx) -> Decision<()> {
        let span = Self::enter_run(ctx);
        let _guard = span.enter();
        self.log_notification(note, "on_notification");
        Decision::Continue
    }

    async fn on_error(&self, ctx: &RunCtx, err: &KernelError) {
        let span = Self::enter_run(ctx);
        let _guard = span.enter();
        let error_text = self.capped_text(&err.to_string());
        error!(
            error = %redact_text(&error_text),
            "kernel error observed",
        );
    }

    async fn on_kernel_shutdown_task_panic(&self, err: &JoinError) {
        let span = hook_span("on_kernel_shutdown_task_panic");
        let _guard = span.enter();
        let error_text = self.capped_text(&err.to_string());
        warn!(
            error = %redact_text(&error_text),
            is_panic = err.is_panic(),
            is_cancelled = err.is_cancelled(),
            "kernel shutdown drain observed task JoinError",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{LogHook, LogHookConfig};

    #[test]
    fn capped_text_truncates_at_utf8_boundary() {
        let max1_hook = LogHook::new().with_config(LogHookConfig {
            max_field_length: 1,
            ..LogHookConfig::default()
        });

        assert_eq!(max1_hook.capped_text("ä"), "");
        assert_eq!(max1_hook.capped_text("äx"), "");
        assert_eq!(max1_hook.capped_text("aäx"), "a");

        let max2_hook = LogHook::new().with_config(LogHookConfig {
            max_field_length: 2,
            ..LogHookConfig::default()
        });

        let value = max2_hook.capped_text("aä");
        assert_eq!(value, "a");
        assert!(value.len() <= 2);

        let max0_hook = LogHook::new().with_config(LogHookConfig {
            max_field_length: 0,
            ..LogHookConfig::default()
        });
        assert_eq!(max0_hook.capped_text("aä"), "");
    }
}
