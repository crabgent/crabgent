use std::collections::VecDeque;

use bytes::Bytes;
use crabgent_core::{
    Citation, EventStream, ProviderError, ProviderEvent, StopReason, ToolCall, Usage,
};
use futures::stream::{self, Stream, StreamExt};
use serde_json::{Value, json};

pub fn into_event_stream(
    bytes_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    cache_creation_tokens: u32,
) -> EventStream {
    let state = StreamState {
        bytes: Box::pin(bytes_stream),
        parser: GeminiSseParser::default(),
        queued: VecDeque::new(),
        finished: false,
        cache_creation_tokens,
        cache_creation_tokens_applied: false,
    };
    Box::pin(stream::unfold(state, advance_stream_state))
}

struct StreamState {
    bytes: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    parser: GeminiSseParser,
    queued: VecDeque<Result<ProviderEvent, ProviderError>>,
    finished: bool,
    cache_creation_tokens: u32,
    cache_creation_tokens_applied: bool,
}

async fn advance_stream_state(
    mut state: StreamState,
) -> Option<(Result<ProviderEvent, ProviderError>, StreamState)> {
    if let Some(event) = state.pop_event() {
        return Some((event, state));
    }
    if state.finished {
        return None;
    }

    while let Some(chunk) = state.bytes.next().await {
        match chunk {
            Ok(bytes) => state.queued.extend(state.parser.feed(&bytes)),
            Err(error) => {
                state.finished = true;
                return Some((Err(ProviderError::Transport(error.to_string())), state));
            }
        }
        if let Some(event) = state.pop_event() {
            return Some((event, state));
        }
    }

    state.finished = true;
    state.queued.extend(state.parser.finish());
    state.pop_event().map(|event| (event, state))
}

impl StreamState {
    fn pop_event(&mut self) -> Option<Result<ProviderEvent, ProviderError>> {
        let mut event = self.queued.pop_front()?;
        if !self.cache_creation_tokens_applied
            && self.cache_creation_tokens > 0
            && let Ok(ProviderEvent::Usage(usage)) = &mut event
        {
            usage.cache_creation_tokens = usage
                .cache_creation_tokens
                .saturating_add(self.cache_creation_tokens);
            self.cache_creation_tokens_applied = true;
        }
        Some(event)
    }
}

#[derive(Default)]
pub struct GeminiSseParser {
    line_buf: String,
    pending_utf8: Vec<u8>,
    saw_stop: bool,
}

impl GeminiSseParser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Result<ProviderEvent, ProviderError>> {
        let mut events = Vec::new();
        if let Err(error) = self.push_utf8_chunk(chunk) {
            events.push(Err(error));
            return events;
        }
        self.drain_lines(&mut events);
        events
    }

    pub fn finish(&mut self) -> Vec<Result<ProviderEvent, ProviderError>> {
        let mut events = Vec::new();
        if !self.pending_utf8.is_empty() {
            self.pending_utf8.clear();
            events.push(Err(ProviderError::MalformedResponse(
                "google stream ended with incomplete utf-8 sequence".to_owned(),
            )));
            return events;
        }
        if !self.line_buf.trim().is_empty() {
            let line = self.line_buf.trim().to_owned();
            self.line_buf.clear();
            self.process_line(&line, &mut events);
        }
        if !self.saw_stop {
            events.push(Ok(ProviderEvent::Stop(StopReason::EndTurn)));
        }
        events
    }

    fn push_utf8_chunk(&mut self, chunk: &[u8]) -> Result<(), ProviderError> {
        self.pending_utf8.extend_from_slice(chunk);
        match std::str::from_utf8(&self.pending_utf8) {
            Ok(decoded) => {
                self.line_buf.push_str(decoded);
                self.pending_utf8.clear();
                Ok(())
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0 {
                    let Some(prefix) = self.pending_utf8.get(..valid) else {
                        return Err(ProviderError::MalformedResponse(
                            "google stream utf-8 prefix was out of bounds".to_owned(),
                        ));
                    };
                    let decoded = std::str::from_utf8(prefix)
                        .map_err(|err| ProviderError::MalformedResponse(err.to_string()))?
                        .to_owned();
                    self.line_buf.push_str(&decoded);
                    self.pending_utf8.drain(..valid);
                }
                if error.error_len().is_some() {
                    self.pending_utf8.clear();
                    return Err(ProviderError::MalformedResponse(
                        "google stream contained invalid utf-8".to_owned(),
                    ));
                }
                Ok(())
            }
        }
    }

    fn drain_lines(&mut self, events: &mut Vec<Result<ProviderEvent, ProviderError>>) {
        while let Some(pos) = self.line_buf.find('\n') {
            let Some(prefix) = self.line_buf.get(..pos) else {
                break;
            };
            let line = prefix.trim_end().to_owned();
            self.line_buf.drain(..=pos);
            self.process_line(&line, events);
        }
    }

    fn process_line(&mut self, line: &str, events: &mut Vec<Result<ProviderEvent, ProviderError>>) {
        let Some(data) = line
            .strip_prefix("data:")
            .map(|rest| rest.strip_prefix(' ').unwrap_or(rest))
        else {
            return;
        };
        if data == "[DONE]" {
            self.saw_stop = true;
            return;
        }
        let parsed = match serde_json::from_str::<Value>(data) {
            Ok(parsed) => parsed,
            Err(error) => {
                events.push(Err(ProviderError::MalformedResponse(error.to_string())));
                return;
            }
        };
        events.extend(events_from_generate_content_chunk(&parsed));
        if chunk_has_finish_reason(&parsed) {
            self.saw_stop = true;
        }
    }
}

fn events_from_generate_content_chunk(value: &Value) -> Vec<Result<ProviderEvent, ProviderError>> {
    let mut events = Vec::new();
    if let Some(candidate) = value
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
    {
        if let Some(parts) = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if part
                        .get("thought")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        events.push(Ok(ProviderEvent::ReasoningDelta(text.to_owned())));
                    } else {
                        events.push(Ok(ProviderEvent::TextDelta(text.to_owned())));
                    }
                }
                if let Some(function_call) = part.get("functionCall")
                    && let Some(call) = tool_call_from_function_call(function_call, part)
                {
                    events.push(Ok(ProviderEvent::ToolUse(call)));
                }
            }
        }
        if let Some(meta) = candidate.get("groundingMetadata") {
            events.push(Ok(grounding_event(meta)));
        }
        if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
            if let Some(usage) = value.get("usageMetadata").map(usage_from_value) {
                events.push(Ok(ProviderEvent::Usage(usage)));
            }
            events.push(Ok(ProviderEvent::Stop(map_finish_reason(reason))));
        }
    }
    if !events
        .iter()
        .any(|event| matches!(event, Ok(ProviderEvent::Usage(_))))
        && let Some(usage) = value.get("usageMetadata").map(usage_from_value)
    {
        events.push(Ok(ProviderEvent::Usage(usage)));
    }
    events
}

fn tool_call_from_function_call(function_call: &Value, part: &Value) -> Option<ToolCall> {
    let name = function_call.get("name").and_then(Value::as_str)?;
    let id = function_call
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(name)
        .to_owned();
    Some(ToolCall {
        id,
        name: name.to_owned(),
        args: function_call
            .get("args")
            .cloned()
            .unwrap_or_else(|| json!({})),
        thought_signature: part
            .get("thoughtSignature")
            .or_else(|| part.get("thought_signature"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn grounding_event(meta: &Value) -> ProviderEvent {
    let citations = meta
        .get("groundingChunks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|chunk| {
            let web = chunk.get("web")?;
            let url = web.get("uri").and_then(Value::as_str)?;
            Some(Citation {
                url: url.to_owned(),
                title: web.get("title").and_then(Value::as_str).map(str::to_owned),
                cited_text: None,
                provider: "google".into(),
                raw: chunk.clone(),
            })
        })
        .collect();
    ProviderEvent::ServerToolResult {
        provider: "google".into(),
        name: "google_search".into(),
        content: meta.clone(),
        citations,
    }
}

fn usage_from_value(value: &Value) -> Usage {
    Usage {
        input_tokens: u32_field(value, "promptTokenCount"),
        output_tokens: u32_field(value, "candidatesTokenCount"),
        cache_creation_tokens: 0,
        cache_read_tokens: u32_field(value, "cachedContentTokenCount"),
    }
}

fn u32_field(value: &Value, field: &str) -> u32 {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default()
}

fn chunk_has_finish_reason(value: &Value) -> bool {
    value
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|candidate| candidate.get("finishReason").is_some())
}

fn map_finish_reason(reason: &str) -> StopReason {
    match reason {
        "MAX_TOKENS" => StopReason::MaxTokens,
        "STOP" => StopReason::EndTurn,
        "MALFORMED_FUNCTION_CALL" => StopReason::ToolUse,
        _ => StopReason::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(line: &str, parser: &mut GeminiSseParser) -> Vec<ProviderEvent> {
        parser
            .feed(line.as_bytes())
            .into_iter()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn parses_text_reasoning_tool_and_stop() {
        let mut parser = GeminiSseParser::default();
        let events = feed(
            concat!(
                "data: {\"candidates\":[{\"content\":{\"parts\":[",
                "{\"text\":\"think\",\"thought\":true},",
                "{\"text\":\"answer\"},",
                "{\"functionCall\":{\"id\":\"call_1\",\"name\":\"lookup\",\"args\":{\"q\":\"x\"}},\"thoughtSignature\":\"sig\"}",
                "]},\"finishReason\":\"MALFORMED_FUNCTION_CALL\"}],",
                "\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":3,\"cachedContentTokenCount\":1}}\n\n"
            ),
            &mut parser,
        );

        assert_reasoning_and_text(&events);
        assert_tool_usage_and_stop(&events);
    }

    #[test]
    fn preserves_split_utf8_across_chunks() {
        let mut parser = GeminiSseParser::default();
        let line = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi €\"}]}}]}\n\n";
        let bytes = line.as_bytes();
        let split = bytes
            .iter()
            .position(|byte| *byte == 0xe2)
            .expect("fixture should contain euro sign");

        assert!(parser.feed(&bytes[..=split]).is_empty());
        let events: Vec<_> = parser
            .feed(&bytes[split + 1..])
            .into_iter()
            .map(Result::unwrap)
            .collect();

        assert!(
            matches!(&events[0], ProviderEvent::TextDelta(text) if text == "hi €"),
            "{events:?}"
        );
    }

    fn assert_reasoning_and_text(events: &[ProviderEvent]) {
        assert!(matches!(
            &events[0],
            ProviderEvent::ReasoningDelta(text) if text == "think"
        ));
        assert!(matches!(
            &events[1],
            ProviderEvent::TextDelta(text) if text == "answer"
        ));
    }

    fn assert_tool_usage_and_stop(events: &[ProviderEvent]) {
        assert!(
            matches!(&events[2], ProviderEvent::ToolUse(call) if call.id == "call_1" && call.name == "lookup" && call.thought_signature.as_deref() == Some("sig"))
        );
        assert!(
            matches!(&events[3], ProviderEvent::Usage(usage) if usage.input_tokens == 2 && usage.output_tokens == 3 && usage.cache_read_tokens == 1)
        );
        assert!(matches!(
            &events[4],
            ProviderEvent::Stop(StopReason::ToolUse)
        ));
        assert_eq!(events.len(), 5);
    }
}
