use crabgent_core::{ProviderEvent, StopReason};
use crabgent_provider_openai::wire::WireFormat;
use crabgent_provider_openai::wire::chat_completions::ChatCompletionsWire;
use crabgent_provider_openai::wire::chat_completions::sse::ChatCompletionsStreamState;
use serde_json::{Value, json};

fn data(value: &Value) -> String {
    format!("data: {value}")
}

fn feed(line: &str, state: &mut ChatCompletionsStreamState) -> Vec<ProviderEvent> {
    let wire = ChatCompletionsWire;
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
fn text_delta_stream() {
    let mut state = ChatCompletionsStreamState::default();
    let first = data(&json!({"choices":[{"delta":{"content":"hel"}}]}));
    let second = data(&json!({"choices":[{"delta":{"content":"lo"}}]}));

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
fn tool_call_delta_accumulates() {
    let mut state = ChatCompletionsStreamState::default();
    let start = data(&json!({"choices":[{"delta":{"tool_calls":[{
        "index": 0,
        "id": "call_1",
        "function": {"name": "search", "arguments": "{\"q\":"}
    }]}}]}));
    let rest = data(&json!({"choices":[{"delta":{"tool_calls":[{
        "index": 0,
        "function": {"arguments": "\"rust\"}"}
    }]}}]}));
    let finish = data(&json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}));

    assert!(feed(&start, &mut state).is_empty());
    assert!(feed(&rest, &mut state).is_empty());
    let events = feed(&finish, &mut state);

    assert!(events.iter().any(|event| {
        matches!(
            event,
            ProviderEvent::ToolUse(call)
                if call.id == "call_1"
                    && call.name == "search"
                    && call.args == json!({"q": "rust"})
        )
    }));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderEvent::Stop(StopReason::ToolUse)))
    );
}

#[test]
fn stop_reason_mapping() {
    let cases = [
        ("stop", StopReason::EndTurn),
        ("length", StopReason::MaxTokens),
        ("tool_calls", StopReason::ToolUse),
    ];

    for (raw, expected) in cases {
        let mut state = ChatCompletionsStreamState::default();
        let event = feed(
            &data(&json!({"choices":[{"delta":{},"finish_reason":raw}]})),
            &mut state,
        )
        .pop()
        .expect("stop event");
        assert!(matches!(event, ProviderEvent::Stop(reason) if reason == expected));
    }
}

#[test]
fn partial_utf8_split_across_chunks() {
    let mut state = ChatCompletionsStreamState::default();
    let first = "data: {\"choices\":[{\"delta\":{\"content\":\"";
    let second = "ä\"}}]}";

    assert!(feed(first, &mut state).is_empty());
    let events = feed(second, &mut state);

    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderEvent::TextDelta(text) if text == "ä"))
    );
}

#[test]
fn done_terminator() {
    let mut state = ChatCompletionsStreamState::default();
    let events = feed("data: [DONE]", &mut state);

    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderEvent::Stop(StopReason::EndTurn)))
    );
}
