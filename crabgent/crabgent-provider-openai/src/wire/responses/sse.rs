//! SSE state for `/backend-api/codex/responses`.

use std::collections::{BTreeMap, HashMap, VecDeque};

use crabgent_core::{Citation, ProviderEvent, StopReason, ToolCall, Usage};
use crabgent_log::warn;
use serde_json::{Value, json};

use crate::wire::responses::{map_stop_reason, parse_arguments};

/// Streaming parser state for Responses.
#[derive(Debug, Clone, Default)]
pub struct ResponsesStreamState {
    pending_data: String,
    function_calls: BTreeMap<usize, FunctionCallBuilder>,
    /// Active web-search-call items indexed by their string id.
    web_search_calls: HashMap<String, WebSearchCallBuilder>,
    queued: VecDeque<ProviderEvent>,
    /// Set when a `response.output_text.delta` arrived in this stream.
    /// The consumer-side `.done` arm only emits the buffered text as a
    /// fallback chunk if no deltas were observed, so backends that
    /// stream both delta and done events (Stainless reference) keep
    /// the per-token UX while backends that emit only the `.done`
    /// event (Codex OAuth at `chatgpt.com/backend-api/codex/responses`)
    /// still surface a one-shot preview chunk.
    text_delta_seen: bool,
    /// Same idea as `text_delta_seen` but for the two reasoning event
    /// families (`response.reasoning_summary_text.delta` and
    /// `response.reasoning_text.delta`).
    reasoning_delta_seen: bool,
}

#[derive(Debug, Clone, Default)]
struct FunctionCallBuilder {
    call_id: String,
    name: String,
    arguments: String,
}

/// Accumulates the JSON for a `web_search_call` output item.
#[derive(Debug, Clone)]
struct WebSearchCallBuilder {
    /// The item JSON as received in `response.output_item.added`.
    item: Value,
}

impl WebSearchCallBuilder {
    const fn new(item: Value) -> Self {
        Self { item }
    }
}

/// Parse one Responses SSE `data:` line or fragment.
pub fn parse_sse_event(line: &str, state: &mut ResponsesStreamState) -> Option<ProviderEvent> {
    if let Some(event) = state.queued.pop_front() {
        return Some(event);
    }

    let data = extract_data(line, !state.pending_data.is_empty())?;
    if data == "[DONE]" {
        state
            .queued
            .push_back(ProviderEvent::Stop(StopReason::EndTurn));
        return state.queued.pop_front();
    }

    state.pending_data.push_str(data);
    let Ok(parsed) = serde_json::from_str::<Value>(&state.pending_data) else {
        return None;
    };
    state.pending_data.clear();
    absorb_event(&parsed, state);
    state.queued.pop_front()
}

/// SSE `event:` lines and other non-data fields carry no payload for
/// the Responses wire: each `data:` JSON already embeds its own
/// `type` field. Drop everything that isn't a `data:` line so
/// `pending_data` doesn't accumulate garbage. The one exception is
/// continuation of an already-started `data:` line whose JSON was
/// split across decoder calls -- treat the next line as a raw payload
/// fragment so the accumulator can complete the JSON.
fn extract_data(line: &str, has_pending: bool) -> Option<&str> {
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(data) = trimmed.strip_prefix("data:") {
        return Some(data.strip_prefix(' ').unwrap_or(data));
    }
    if has_pending {
        return Some(trimmed);
    }
    None
}

fn absorb_event(parsed: &Value, state: &mut ResponsesStreamState) {
    match parsed.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => absorb_text_delta(parsed, state),
        // OpenAI streams reasoning as two parallel event types: the
        // public-summary `reasoning_summary_text.delta` and the verbose
        // `reasoning_text.delta`. Both carry a `delta` string. The
        // `content_index` field on `reasoning_text.delta` is intentionally
        // ignored: `ProviderEvent::ReasoningDelta` is index-less by
        // design (cross-provider neutral, see SD-04 in Reasoning-event).
        Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
            absorb_reasoning_delta(parsed, state);
        }
        // Done-event fallbacks: backends that buffer text/reasoning
        // and emit a single `.done` event with the full payload (Codex
        // OAuth at chatgpt.com/backend-api/codex/responses observed
        // for both `output_text.done` and `reasoning_*.done`) still
        // get a preview chunk surfaced. Guarded by the per-stream
        // `*_delta_seen` flags so the public Stainless event flow
        // (delta + done) does not double-emit.
        Some("response.output_text.done") => absorb_text_done(parsed, state),
        Some("response.reasoning_summary_text.done" | "response.reasoning_text.done") => {
            absorb_reasoning_done(parsed, state);
        }
        Some("response.function_call_arguments.delta") => absorb_arguments_delta(parsed, state),
        Some("response.output_item.added") => absorb_output_item_added(parsed, state),
        Some("response.output_item.done") => absorb_output_item_done(parsed, state),
        Some("response.completed") => absorb_completed(parsed, state),
        // Diagnostic: any unknown event type lands here. Backends that
        // do not follow the public Stainless event-name set (Codex
        // OAuth on chatgpt.com/backend-api/codex/responses notably
        // ships a different vocabulary) appear in the trace below so
        // an operator can tell at a glance whether the parser is
        // missing arms.
        other => {
            crabgent_log::debug!(
                event_type = ?other,
                "responses-sse: unhandled event type"
            );
        }
    }
}

fn absorb_reasoning_delta(parsed: &Value, state: &mut ResponsesStreamState) {
    if let Some(delta) = parsed.get("delta").and_then(Value::as_str)
        && !delta.is_empty()
    {
        state.reasoning_delta_seen = true;
        state
            .queued
            .push_back(ProviderEvent::ReasoningDelta(delta.to_owned()));
    } else {
        crabgent_log::debug!(
            payload = ?parsed,
            "responses-sse: reasoning delta event without usable delta field"
        );
    }
}

fn absorb_text_delta(parsed: &Value, state: &mut ResponsesStreamState) {
    if let Some(delta) = parsed.get("delta").and_then(Value::as_str)
        && !delta.is_empty()
    {
        state.text_delta_seen = true;
        state
            .queued
            .push_back(ProviderEvent::TextDelta(delta.to_owned()));
    } else {
        crabgent_log::debug!(
            payload = ?parsed,
            "responses-sse: text delta event without usable delta field"
        );
    }
}

fn absorb_text_done(parsed: &Value, state: &mut ResponsesStreamState) {
    if state.text_delta_seen {
        return;
    }
    if let Some(text) = parsed.get("text").and_then(Value::as_str)
        && !text.is_empty()
    {
        state
            .queued
            .push_back(ProviderEvent::TextDelta(text.to_owned()));
    }
}

fn absorb_reasoning_done(parsed: &Value, state: &mut ResponsesStreamState) {
    if state.reasoning_delta_seen {
        return;
    }
    if let Some(text) = parsed.get("text").and_then(Value::as_str)
        && !text.is_empty()
    {
        state
            .queued
            .push_back(ProviderEvent::ReasoningDelta(text.to_owned()));
    }
}

fn absorb_arguments_delta(parsed: &Value, state: &mut ResponsesStreamState) {
    // Fall back to index 0 (the common single-tool-call case) when the event
    // carries no usable `output_index`, so argument deltas are accumulated
    // rather than silently dropped.
    let index = parse_output_index(parsed).unwrap_or_else(|| {
        warn!(
            event_type = "response.function_call_arguments.delta",
            "openai responses stream got function-call arguments delta without valid output index; accumulating into index 0"
        );
        0
    });
    let builder = state.function_calls.entry(index).or_default();
    if let Some(call_id) = parsed.get("call_id").and_then(Value::as_str) {
        call_id.clone_into(&mut builder.call_id);
    }
    if let Some(name) = parsed.get("name").and_then(Value::as_str) {
        name.clone_into(&mut builder.name);
    }
    if let Some(delta) = parsed.get("delta").and_then(Value::as_str) {
        builder.arguments.push_str(delta);
    }
}

/// Handle `response.output_item.added` events.
///
/// When the item type is `web_search_call`, register it in the state so the
/// matching `.done` event can emit `ProviderEvent::ServerToolResult`.
fn absorb_output_item_added(parsed: &Value, state: &mut ResponsesStreamState) {
    let item = parsed.get("item").unwrap_or(parsed);
    if item.get("type").and_then(Value::as_str) != Some("web_search_call") {
        return;
    }
    let Some(id) = item.get("id").and_then(Value::as_str) else {
        warn!(
            event_type = "response.output_item.added",
            "openai responses stream: web_search_call item missing id, skipping"
        );
        return;
    };
    state
        .web_search_calls
        .insert(id.to_owned(), WebSearchCallBuilder::new(item.clone()));
}

fn absorb_output_item_done(parsed: &Value, state: &mut ResponsesStreamState) {
    let item = parsed.get("item").unwrap_or(parsed);
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => absorb_function_call_done(parsed, item, state),
        Some("web_search_call") => absorb_web_search_call_done(item, state),
        _ => {}
    }
}

fn absorb_function_call_done(parsed: &Value, item: &Value, state: &mut ResponsesStreamState) {
    let Some(index) = parse_output_index(parsed) else {
        warn!(
            event_type = "response.output_item.done",
            "openai responses stream skipped malformed function-call event without valid output index"
        );
        return;
    };
    let mut builder = state.function_calls.remove(&index).unwrap_or_default();
    if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
        call_id.clone_into(&mut builder.call_id);
    }
    if let Some(name) = item.get("name").and_then(Value::as_str) {
        name.clone_into(&mut builder.name);
    }
    if let Some(arguments) = item.get("arguments").and_then(Value::as_str)
        && builder.arguments.is_empty()
    {
        builder.arguments.push_str(arguments);
    }
    state.queued.push_back(ProviderEvent::ToolUse(ToolCall {
        id: builder.call_id,
        name: builder.name,
        args: parse_arguments(&builder.arguments).unwrap_or_else(|_| json!({})),
        thought_signature: None,
    }));
}

/// Emit `ProviderEvent::ServerToolResult` for a completed web-search call.
///
/// Citations are not available on the `web_search_call` item itself; they
/// appear as `url_citation` annotations on the subsequent `output_text`
/// content items in the response message. This function emits the result
/// without citations because they have not arrived yet at this point in
/// the stream. The annotations are extracted separately in
/// `absorb_text_done` / text content processing when that feature is
/// needed (see the `url_citation` arm in `extract_url_citations`).
fn absorb_web_search_call_done(item: &Value, state: &mut ResponsesStreamState) {
    let Some(id) = item.get("id").and_then(Value::as_str) else {
        warn!(
            event_type = "response.output_item.done",
            "openai responses stream: web_search_call done item missing id, skipping"
        );
        return;
    };
    // Remove the builder registered on `added`; fall back to the done-item
    // itself if `added` was not observed (backends that skip `added`).
    let content = state
        .web_search_calls
        .remove(id)
        .map_or_else(|| item.clone(), |b| b.item);

    state.queued.push_back(ProviderEvent::ServerToolResult {
        provider: "openai".into(),
        name: "web_search".into(),
        content,
        citations: Vec::new(),
    });
}

fn absorb_completed(parsed: &Value, state: &mut ResponsesStreamState) {
    // Extract url_citation annotations from the response output items when
    // the full response is available. This handles the case where
    // annotations land in the completed event's response object.
    if let Some(response) = parsed.get("response") {
        extract_and_queue_citations(response, state);
        if let Some(usage) = response.get("usage") {
            state
                .queued
                .push_back(ProviderEvent::Usage(parse_usage(usage)));
        }
        let status = response.get("status").and_then(Value::as_str);
        state
            .queued
            .push_back(ProviderEvent::Stop(map_stop_reason(status)));
    }
}

/// Walk the response `output` array and emit `url_citation` annotations as
/// `ProviderEvent::ServerToolResult` events so callers receive citations
/// attached to the text output.
fn extract_and_queue_citations(response: &Value, state: &mut ResponsesStreamState) {
    let Some(output) = response.get("output").and_then(Value::as_array) else {
        return;
    };
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) != Some("output_text") {
                continue;
            }
            let Some(annotations) = block.get("annotations").and_then(Value::as_array) else {
                continue;
            };
            let citations: Vec<Citation> =
                annotations.iter().filter_map(parse_url_citation).collect();
            if !citations.is_empty() {
                state.queued.push_back(ProviderEvent::ServerToolResult {
                    provider: "openai".into(),
                    name: "web_search".into(),
                    content: block.clone(),
                    citations,
                });
            }
        }
    }
}

/// Parse a single `url_citation` annotation into a `Citation`.
///
/// Returns `None` with a warn log when a required field (`url`) is missing.
fn parse_url_citation(annotation: &Value) -> Option<Citation> {
    if annotation.get("type").and_then(Value::as_str) != Some("url_citation") {
        return None;
    }
    let citation_obj = annotation.get("url_citation").or(Some(annotation))?;
    let url = if let Some(u) = citation_obj.get("url").and_then(Value::as_str) {
        u.to_owned()
    } else {
        warn!(
            raw = ?annotation,
            "openai responses-sse: url_citation annotation missing url, skipping"
        );
        return None;
    };
    let title = citation_obj
        .get("title")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Some(Citation {
        url,
        title,
        cited_text: None,
        provider: "openai".into(),
        raw: annotation.clone(),
    })
}

fn parse_output_index(parsed: &Value) -> Option<usize> {
    parsed
        .get("output_index")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

fn parse_usage(usage: &Value) -> Usage {
    Usage {
        input_tokens: get_u32(usage, "input_tokens"),
        output_tokens: get_u32(usage, "output_tokens"),
        cache_creation_tokens: 0,
        cache_read_tokens: usage
            .get("input_tokens_details")
            .map_or(0, |details| get_u32(details, "cached_tokens")),
    }
}

fn get_u32(value: &Value, key: &str) -> u32 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}
