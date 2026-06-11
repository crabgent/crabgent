use crabgent_core::{ProviderEvent, StopReason};
use crabgent_provider_openai::wire::WireFormat;
use crabgent_provider_openai::wire::responses::ResponsesWire;
use crabgent_provider_openai::wire::responses::sse::ResponsesStreamState;
use serde_json::{Value, json};

fn data(value: &Value) -> String {
    format!("data: {value}")
}

fn feed(line: &str, state: &mut ResponsesStreamState) -> Vec<ProviderEvent> {
    let wire = ResponsesWire;
    let mut events = Vec::new();
    if let Some(event) = wire.parse_sse_event(line, state) {
        events.push(event);
    }
    while let Some(event) = wire.parse_sse_event("", state) {
        events.push(event);
    }
    events
}

#[test]
fn output_text_delta_stream() {
    let mut state = ResponsesStreamState::default();
    let first = data(&json!({"type":"response.output_text.delta","delta":"hel"}));
    let second = data(&json!({"type":"response.output_text.delta","delta":"lo"}));

    let mut text = String::new();
    for event in feed(&first, &mut state)
        .into_iter()
        .chain(feed(&second, &mut state))
    {
        if let ProviderEvent::TextDelta(delta) = event {
            text.push_str(&delta);
        }
    }

    assert_eq!(text, "hello");
}

#[test]
fn function_call_arguments_delta_accumulates() {
    let mut state = ResponsesStreamState::default();
    let first = data(&json!({
        "type": "response.function_call_arguments.delta",
        "output_index": 0,
        "call_id": "call_1",
        "name": "search",
        "delta": "{\"q\":"
    }));
    let second = data(&json!({
        "type": "response.function_call_arguments.delta",
        "output_index": 0,
        "delta": "\"rust\"}"
    }));
    let done = data(&json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": {"type": "function_call", "call_id": "call_1", "name": "search"}
    }));

    assert!(feed(&first, &mut state).is_empty());
    assert!(feed(&second, &mut state).is_empty());
    let events = feed(&done, &mut state);

    assert!(events.iter().any(|event| {
        matches!(
            event,
            ProviderEvent::ToolUse(call)
                if call.id == "call_1"
                    && call.name == "search"
                    && call.args == json!({"q": "rust"})
        )
    }));
}

#[test]
fn function_call_delta_without_index_accumulates_into_index_zero() {
    // A single-tool-call stream whose argument deltas omit `output_index`.
    // The deltas must accumulate into index 0 (the common case), not be
    // dropped, so the matching `.done` event recovers the full arguments.
    let mut state = ResponsesStreamState::default();
    let first = data(&json!({
        "type": "response.function_call_arguments.delta",
        "call_id": "call_1",
        "name": "search",
        "delta": "{\"q\":"
    }));
    let second = data(&json!({
        "type": "response.function_call_arguments.delta",
        "delta": "\"rust\"}"
    }));
    let done = data(&json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": {"type": "function_call", "call_id": "call_1", "name": "search"}
    }));

    assert!(feed(&first, &mut state).is_empty());
    assert!(feed(&second, &mut state).is_empty());
    let events = feed(&done, &mut state);

    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                ProviderEvent::ToolUse(call)
                    if call.id == "call_1"
                        && call.name == "search"
                        && call.args == json!({"q": "rust"})
            )
        }),
        "no-index argument deltas accumulate into index 0 instead of being dropped"
    );
}

#[test]
fn function_call_done_without_index_is_skipped() {
    let mut state = ResponsesStreamState::default();
    let valid_delta = data(&json!({
        "type": "response.function_call_arguments.delta",
        "output_index": 0,
        "call_id": "call_1",
        "name": "search",
        "delta": "{\"q\":\"rust\"}"
    }));
    let malformed_done = data(&json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "call_id": "call_bad",
            "name": "bad",
            "arguments": "{\"q\":\"wrong\"}"
        }
    }));
    let valid_done = data(&json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": {"type": "function_call", "call_id": "call_1", "name": "search"}
    }));

    assert!(feed(&valid_delta, &mut state).is_empty());
    assert!(feed(&malformed_done, &mut state).is_empty());
    let events = feed(&valid_done, &mut state);

    assert!(events.iter().any(|event| {
        matches!(
            event,
            ProviderEvent::ToolUse(call)
                if call.id == "call_1"
                    && call.name == "search"
                    && call.args == json!({"q": "rust"})
        )
    }));
}

#[test]
fn response_completed_event_stop_reason() {
    let mut state = ResponsesStreamState::default();
    let events = feed(
        &data(&json!({
            "type": "response.completed",
            "response": {
                "status": "completed",
                "usage": {
                    "input_tokens": 4,
                    "output_tokens": 2,
                    "input_tokens_details": {"cached_tokens": 1}
                }
            }
        })),
        &mut state,
    );

    assert!(
        events.iter().any(
            |event| matches!(event, ProviderEvent::Usage(usage) if usage.cache_read_tokens == 1)
        )
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderEvent::Stop(StopReason::EndTurn)))
    );
}

#[test]
fn reasoning_summary_text_delta_stream() {
    let mut state = ResponsesStreamState::default();
    let first = data(&json!({
        "type": "response.reasoning_summary_text.delta",
        "delta": "Let me think..."
    }));

    let events = feed(&first, &mut state);

    assert!(
        events.iter().any(|event| matches!(
            event,
            ProviderEvent::ReasoningDelta(detail) if detail == "Let me think..."
        )),
        "ReasoningDelta forwarded from reasoning_summary_text.delta"
    );
}

#[test]
fn reasoning_text_delta_stream() {
    let mut state = ResponsesStreamState::default();
    let first = data(&json!({
        "type": "response.reasoning_text.delta",
        "content_index": 0,
        "delta": "step one"
    }));

    let events = feed(&first, &mut state);

    assert!(
        events.iter().any(|event| matches!(
            event,
            ProviderEvent::ReasoningDelta(detail) if detail == "step one"
        )),
        "ReasoningDelta forwarded from reasoning_text.delta, content_index ignored by design"
    );
}

#[test]
fn mixed_reasoning_and_output_stream() {
    let mut state = ResponsesStreamState::default();
    let reasoning = data(&json!({
        "type": "response.reasoning_summary_text.delta",
        "delta": "think"
    }));
    let output = data(&json!({
        "type": "response.output_text.delta",
        "delta": "answer"
    }));
    let completed = data(&json!({
        "type": "response.completed",
        "response": {"status": "completed"}
    }));

    let mut events = feed(&reasoning, &mut state);
    events.extend(feed(&output, &mut state));
    events.extend(feed(&completed, &mut state));

    assert!(
        events.iter().any(|event| matches!(
            event,
            ProviderEvent::ReasoningDelta(detail) if detail == "think"
        )),
        "reasoning delta emitted alongside output delta without cross-contamination"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            ProviderEvent::TextDelta(detail) if detail == "answer"
        )),
        "text delta stays distinct from reasoning delta"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderEvent::Stop(StopReason::EndTurn)))
    );
}

#[test]
fn partial_utf8_split() {
    let mut state = ResponsesStreamState::default();
    let first = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"";
    let second = "ä\"}";

    assert!(feed(first, &mut state).is_empty());
    let events = feed(second, &mut state);

    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderEvent::TextDelta(text) if text == "ä"))
    );
}

#[test]
fn web_search_call_added_and_done_emits_server_tool_result() {
    let mut state = ResponsesStreamState::default();

    let added = data(&json!({
        "type": "response.output_item.added",
        "item": {
            "type": "web_search_call",
            "id": "ws_01",
            "action": {"type": "search", "query": "rust tokio async"}
        }
    }));
    let done = data(&json!({
        "type": "response.output_item.done",
        "item": {
            "type": "web_search_call",
            "id": "ws_01",
            "action": {"type": "search", "query": "rust tokio async"},
            "status": "completed"
        }
    }));

    // added event: nothing emitted yet
    assert!(
        feed(&added, &mut state).is_empty(),
        "added event should not emit immediately"
    );
    // done event: emits ServerToolResult
    let events = feed(&done, &mut state);
    assert!(
        events.iter().any(|e| matches!(
            e,
            ProviderEvent::ServerToolResult { provider, name, .. }
            if provider == "openai" && name == "web_search"
        )),
        "ServerToolResult must be emitted on web_search_call done"
    );
}

#[test]
fn web_search_call_done_without_added_emits_server_tool_result() {
    let mut state = ResponsesStreamState::default();

    // Backends that skip `added` events: only `done` arrives.
    let done = data(&json!({
        "type": "response.output_item.done",
        "item": {
            "type": "web_search_call",
            "id": "ws_02",
            "status": "completed"
        }
    }));

    let events = feed(&done, &mut state);
    assert!(
        events.iter().any(|e| matches!(
            e,
            ProviderEvent::ServerToolResult { provider, name, .. }
            if provider == "openai" && name == "web_search"
        )),
        "ServerToolResult must be emitted even without prior added event"
    );
}

#[test]
fn completed_event_extracts_url_citations() {
    let mut state = ResponsesStreamState::default();

    let annotation = json!({
        "type": "url_citation",
        "url_citation": {
            "url": "https://example.com/rust",
            "title": "Rust Programming Language"
        }
    });

    let completed = data(&json!({
        "type": "response.completed",
        "response": {
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Rust is great.",
                            "annotations": [annotation]
                        }
                    ]
                }
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        }
    }));

    let events = feed(&completed, &mut state);

    // Must find a ServerToolResult with the citation
    let citation_event = events.iter().find(|e| {
        matches!(e, ProviderEvent::ServerToolResult { provider, name, citations, .. }
            if provider == "openai"
                && name == "web_search"
                && !citations.is_empty()
        )
    });
    assert!(
        citation_event.is_some(),
        "url_citation annotation must become ServerToolResult"
    );

    if let Some(ProviderEvent::ServerToolResult { citations, .. }) = citation_event {
        assert_eq!(citations[0].url, "https://example.com/rust");
        assert_eq!(
            citations[0].title.as_deref(),
            Some("Rust Programming Language")
        );
        assert_eq!(citations[0].provider, "openai");
    }
}
