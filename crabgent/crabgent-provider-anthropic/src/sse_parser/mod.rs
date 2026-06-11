//! Anthropic SSE streaming protocol parser. Converts raw SSE bytes into
//! `crabgent_core::ProviderEvent` values.
//!
//! Tracks block builders per index. Text blocks emit a `TextDelta` per
//! `text_delta` event; thinking blocks emit a `ReasoningDelta` per
//! `thinking_delta` event; tool blocks accumulate `input_json` and emit a
//! single `ToolUse` on `content_block_stop`. `server_tool_use` blocks
//! accumulate the search query; `web_search_tool_result` blocks accumulate
//! the full result JSON (including `encrypted_content`) and emit a single
//! `ProviderEvent::ServerToolResult` on `content_block_stop`.

mod web_search;

use std::collections::HashMap;

use crate::retry::is_retryable_stream_error;
use crabgent_core::text::truncate_bytes_at_boundary;
use crabgent_core::{ProviderEvent, Usage};
use crabgent_log::{debug, warn};
use serde_json::{Value, json};

use web_search::{WebSearchResultBuilder, web_search_result_builder};

const MAX_LINE_LENGTH: usize = 2 * 1024 * 1024;
const MAX_BLOCK_BUILDERS: usize = 100;
const UTF8_REMAINDER_CAP: usize = 4;
const DEFAULT_BLOCK_CONTENT_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_TOTAL_CONTENT_BYTES: usize = 32 * 1024 * 1024;

pub(crate) type ParserResult = Result<ProviderEvent, SseError>;

/// Parser-level stream error with retryability metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseError {
    message: String,
    retryable: bool,
}

impl SseError {
    fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
    fn stream(message: impl Into<String>, retryable: bool) -> Self {
        Self {
            message: message.into(),
            retryable,
        }
    }
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        self.retryable
    }
}

/// Per-block size limits applied while accumulating SSE content.
///
/// `total_content_bytes` is a SHARED budget across every block kind
/// the parser produces: text deltas, thinking deltas, and tool-input
/// JSON. A long reasoning stream therefore silently displaces text
/// or tool-input budget within the same response; the default 32 MB
/// cap (`DEFAULT_TOTAL_CONTENT_BYTES`) keeps that displacement
/// harmless in practice, but custom callers that tighten the budget
/// must accept the silent-drop behaviour or set `block_content_bytes`
/// instead, which caps each block in isolation.
#[derive(Debug, Clone, Copy)]
pub struct ParserLimits {
    pub block_content_bytes: usize,
    pub total_content_bytes: usize,
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self {
            block_content_bytes: DEFAULT_BLOCK_CONTENT_BYTES,
            total_content_bytes: DEFAULT_TOTAL_CONTENT_BYTES,
        }
    }
}

enum BlockBuilder {
    Text(String),
    Thinking(String),
    ToolUse {
        id: String,
        name: String,
        input_json: String,
        poisoned: bool,
    },
    ServerToolUse,
    WebSearchResult(WebSearchResultBuilder),
}

/// SSE parser state. `feed()` pushes bytes; `finish()` flushes any
/// dangling state when the stream closes.
pub struct SseParser {
    line_buf: String,
    current_event_type: String,
    block_builders: HashMap<usize, BlockBuilder>,
    utf8_remainder: Vec<u8>,
    total_content_bytes: usize,
    stop_reason_raw: String,
    usage: Usage,
    limits: ParserLimits,
    finalized_stop: bool,
    /// Configured API key, used to redact server-controlled error-event
    /// messages before they surface in an `SseError`. Empty when unset, in
    /// which case redaction still strips secret-like spans by pattern.
    api_key: String,
}

impl SseParser {
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(ParserLimits::default())
    }
    #[must_use]
    pub fn with_limits(limits: ParserLimits) -> Self {
        Self {
            line_buf: String::new(),
            current_event_type: String::new(),
            block_builders: HashMap::new(),
            utf8_remainder: Vec::new(),
            total_content_bytes: 0,
            stop_reason_raw: String::new(),
            usage: Usage::default(),
            limits,
            finalized_stop: false,
            api_key: String::new(),
        }
    }

    /// Set the API key used to redact stream error-event messages.
    #[must_use]
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = api_key.into();
        self
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<ParserResult> {
        let mut events = Vec::new();
        let decoded = self.decode_utf8_safe(chunk);
        self.line_buf.push_str(&decoded);
        if self.line_buf.len() > MAX_LINE_LENGTH {
            self.reset_on_overflow(&mut events);
            return events;
        }
        while let Some(pos) = self.line_buf.find('\n') {
            let line = self
                .line_buf
                .get(..pos)
                .unwrap_or_default()
                .trim_end()
                .to_string();
            self.line_buf.drain(..=pos);
            self.process_line(&line, &mut events);
        }
        events
    }
    pub fn finish(mut self) -> Vec<ParserResult> {
        let mut events = Vec::new();
        if !self.finalized_stop {
            let stop = map_stop_reason(&self.stop_reason_raw);
            events.push(Ok(ProviderEvent::Usage(self.usage)));
            events.push(Ok(ProviderEvent::Stop(stop)));
        }
        // Drop any unfinished builders silently; they had no content_block_stop.
        self.block_builders.clear();
        events
    }

    fn process_line(&mut self, line: &str, events: &mut Vec<ParserResult>) {
        if line.is_empty() {
            self.current_event_type.clear();
            return;
        }
        if let Some(event_type) = line.strip_prefix("event: ") {
            self.current_event_type = event_type.to_string();
            return;
        }
        let Some(data) = line
            .strip_prefix("data:")
            .map(|rest| rest.strip_prefix(' ').unwrap_or(rest))
        else {
            return;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(data) else {
            debug!(
                prefix = truncate_bytes_at_boundary(data, 80),
                "SSE: dropped non-JSON data frame"
            );
            return;
        };
        self.dispatch_event(&parsed, events);
    }

    fn dispatch_event(&mut self, parsed: &Value, events: &mut Vec<ParserResult>) {
        match self.current_event_type.as_str() {
            "content_block_start" => self.handle_block_start(parsed, events),
            "content_block_delta" => {
                if let Some(ev) = self.handle_block_delta(parsed) {
                    events.push(ev);
                }
            }
            "content_block_stop" => {
                if let Some(ev) = self.handle_block_stop(parsed) {
                    events.push(ev);
                }
            }
            "message_start" => self.absorb_message_start(parsed),
            "message_delta" => self.absorb_message_delta(parsed, events),
            "error" => self.absorb_error(parsed, events),
            _ => {}
        }
    }

    fn handle_block_start(&mut self, parsed: &Value, events: &mut Vec<ParserResult>) {
        let Some(index) = parse_index(parsed) else {
            return;
        };
        if self.block_builders.len() >= MAX_BLOCK_BUILDERS
            && !self.block_builders.contains_key(&index)
        {
            warn!(
                index,
                max = MAX_BLOCK_BUILDERS,
                "SSE: max content block builders reached"
            );
            events.push(Err(SseError::fatal(format!(
                "max content block builders reached: {MAX_BLOCK_BUILDERS}"
            ))));
            return;
        }
        let empty = json!({});
        let block = parsed.get("content_block").unwrap_or(&empty);
        let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
        match block_type {
            "text" => {
                self.block_builders
                    .insert(index, BlockBuilder::Text(String::new()));
            }
            "tool_use" => {
                let id = block_field(block, "id");
                let name = block_field(block, "name");
                self.block_builders.insert(
                    index,
                    BlockBuilder::ToolUse {
                        id,
                        name,
                        input_json: String::new(),
                        poisoned: false,
                    },
                );
            }
            "thinking" => {
                self.block_builders
                    .insert(index, BlockBuilder::Thinking(String::new()));
            }
            "server_tool_use" => {
                // Register presence; stop produces no event (result block follows).
                self.block_builders
                    .insert(index, BlockBuilder::ServerToolUse);
            }
            "web_search_tool_result" => {
                self.block_builders.insert(
                    index,
                    BlockBuilder::WebSearchResult(web_search_result_builder(block)),
                );
            }
            _ => {}
        }
    }

    fn handle_block_delta(&mut self, parsed: &Value) -> Option<ParserResult> {
        let index = parse_index(parsed)?;
        let empty = json!({});
        let delta = parsed.get("delta").unwrap_or(&empty);
        let delta_type = delta.get("type").and_then(Value::as_str).unwrap_or("");
        match delta_type {
            "text_delta" => self.handle_text_delta(index, delta),
            "thinking_delta" => self.handle_thinking_delta(index, delta),
            "input_json_delta" => self.handle_input_json_delta(index, delta),
            _ => None,
        }
    }

    fn handle_text_delta(&mut self, index: usize, delta: &Value) -> Option<ParserResult> {
        let text = delta.get("text").and_then(Value::as_str)?;
        let Some(BlockBuilder::Text(buf)) = self.block_builders.get_mut(&index) else {
            return None;
        };
        if !append_within_limits(buf, text, &self.limits, &mut self.total_content_bytes) {
            return None;
        }
        Some(Ok(ProviderEvent::TextDelta(text.to_string())))
    }

    fn handle_thinking_delta(&mut self, index: usize, delta: &Value) -> Option<ParserResult> {
        let text = delta.get("thinking").and_then(Value::as_str)?;
        let Some(BlockBuilder::Thinking(buf)) = self.block_builders.get_mut(&index) else {
            return None;
        };
        if !append_within_limits(buf, text, &self.limits, &mut self.total_content_bytes) {
            return None;
        }
        Some(Ok(ProviderEvent::ReasoningDelta(text.to_string())))
    }

    fn handle_input_json_delta(&mut self, index: usize, delta: &Value) -> Option<ParserResult> {
        let partial = delta.get("partial_json").and_then(Value::as_str)?;
        match self.block_builders.get_mut(&index)? {
            BlockBuilder::ToolUse {
                input_json,
                poisoned,
                ..
            } => {
                if !append_within_limits(
                    input_json,
                    partial,
                    &self.limits,
                    &mut self.total_content_bytes,
                ) {
                    *poisoned = true;
                    return Some(Err(SseError::fatal(
                        "tool call input truncated: exceeds size limit",
                    )));
                }
                None
            }
            _ => None,
        }
    }

    fn handle_block_stop(&mut self, parsed: &Value) -> Option<ParserResult> {
        let index = parse_index(parsed)?;
        let builder = self.block_builders.remove(&index)?;
        match builder {
            BlockBuilder::Text(_) | BlockBuilder::Thinking(_) | BlockBuilder::ServerToolUse => None,
            BlockBuilder::ToolUse {
                id,
                name,
                input_json,
                poisoned,
            } => Some(finalize_tool_use(id, name, &input_json, poisoned)),
            BlockBuilder::WebSearchResult(b) => Some(Ok(b.finalize())),
        }
    }

    fn absorb_message_start(&mut self, parsed: &Value) {
        let Some(usage) = parsed.get("message").and_then(|m| m.get("usage")) else {
            return;
        };
        self.usage.input_tokens = usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .map_or(self.usage.input_tokens, |n| {
                u32::try_from(n).unwrap_or(u32::MAX)
            });
        self.usage.cache_creation_tokens = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .map_or(self.usage.cache_creation_tokens, |n| {
                u32::try_from(n).unwrap_or(u32::MAX)
            });
        self.usage.cache_read_tokens = usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .map_or(self.usage.cache_read_tokens, |n| {
                u32::try_from(n).unwrap_or(u32::MAX)
            });
    }

    fn absorb_message_delta(&mut self, parsed: &Value, events: &mut Vec<ParserResult>) {
        let error_committed = is_error_stop_reason(&self.stop_reason_raw);
        if !error_committed
            && let Some(sr) = parsed
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(Value::as_str)
        {
            self.stop_reason_raw = sr.to_string();
        }
        if let Some(usage) = parsed.get("usage") {
            self.usage.output_tokens = usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .map_or(self.usage.output_tokens, |n| {
                    u32::try_from(n).unwrap_or(u32::MAX)
                });
        }
        // Emit Usage + Stop terminal events here so consumers see them
        // even if the connection drops before message_stop.
        if !self.stop_reason_raw.is_empty() && !self.finalized_stop {
            self.finalized_stop = true;
            events.push(Ok(ProviderEvent::Usage(self.usage)));
            events.push(Ok(ProviderEvent::Stop(map_stop_reason(
                &self.stop_reason_raw,
            ))));
        }
    }

    fn absorb_error(&mut self, parsed: &Value, events: &mut Vec<ParserResult>) {
        let err = parsed.get("error").unwrap_or(parsed);
        let kind = err.get("type").and_then(Value::as_str).unwrap_or("unknown");
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        self.stop_reason_raw = format!("error:{kind}");
        let retryable = is_retryable_stream_error(&self.stop_reason_raw).is_some();
        // The error message is server-controlled and may echo request
        // fragments that contain the API key. Route it through the same
        // redaction as the HTTP error path before it reaches the caller or
        // the LLM. The full detail stays in operator logs only.
        let redacted = crate::redact::redact_error_body(msg, &self.api_key);
        warn!(
            error_type = kind,
            detail = redacted.as_str(),
            "anthropic stream error event"
        );
        events.push(Err(SseError::stream(
            format!("{kind}: {redacted}"),
            retryable,
        )));
    }

    fn reset_on_overflow(&mut self, events: &mut Vec<ParserResult>) {
        self.line_buf.clear();
        self.current_event_type.clear();
        self.block_builders.clear();
        self.stop_reason_raw = "error".into();
        events.push(Err(SseError::fatal("SSE line buffer overflow")));
    }

    fn decode_utf8_safe(&mut self, chunk: &[u8]) -> String {
        if self.utf8_remainder.is_empty() {
            return match std::str::from_utf8(chunk) {
                Ok(s) => s.to_string(),
                Err(e) => {
                    let valid = e.valid_up_to();
                    self.utf8_remainder
                        .extend_from_slice(chunk.get(valid..).unwrap_or_default());
                    String::from_utf8_lossy(chunk.get(..valid).unwrap_or_default()).into_owned()
                }
            };
        }
        let mut buf = std::mem::take(&mut self.utf8_remainder);
        buf.extend_from_slice(chunk);
        match String::from_utf8(buf) {
            Ok(s) => s,
            Err(e) => {
                let valid_up_to = e.utf8_error().valid_up_to();
                let mut bytes = e.into_bytes();
                self.utf8_remainder = bytes.split_off(valid_up_to);
                if self.utf8_remainder.len() > UTF8_REMAINDER_CAP {
                    let flushed = std::mem::take(&mut self.utf8_remainder);
                    let flushed_lossy = String::from_utf8_lossy(&flushed).into_owned();
                    let decoded = String::from_utf8_lossy(&bytes).into_owned();
                    return format!("{decoded}{flushed_lossy}");
                }
                String::from_utf8_lossy(&bytes).into_owned()
            }
        }
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

mod helpers;
use helpers::{
    append_within_limits, block_field, finalize_tool_use, is_error_stop_reason, map_stop_reason,
    parse_index,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod web_search_tests;
