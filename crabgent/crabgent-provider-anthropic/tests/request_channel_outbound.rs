use crabgent_core::LlmRequest;
use crabgent_provider_anthropic::request::build_body;
use serde_json::{Value, json};

fn req(messages: Vec<Value>) -> LlmRequest {
    LlmRequest {
        model: "claude-sonnet-4-5".into(),
        system_prompt: Some("be terse".into()),
        messages,
        tools: vec![],
        max_tokens: Some(2048),
        temperature: Some(0.5),
        stop_sequences: vec![],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    }
}

fn build_test_body(req: &LlmRequest, stream: bool, cache_ttl: Option<&str>) -> Value {
    build_body(req, stream, cache_ttl, req.model.as_str()).expect("build_body")
}

fn channel_outbound(body: &str) -> Value {
    json!({
        "role": "channel_outbound",
        "conv": "matrix:room",
        "body": body,
        "channel": "matrix",
        "message_id": "event-id",
        "thread_root": null,
        "broadcast": false,
    })
}

fn user_message(text: &str) -> Value {
    json!({"role": "user", "content": [{"type": "text", "text": text}]})
}

fn assistant_tool_use() -> Value {
    json!({
        "role": "assistant",
        "text": "",
        "tool_calls": [{"id": "call-1", "name": "channel_send", "args": {"body": "posted"}}],
    })
}

fn tool_result() -> Value {
    json!({
        "role": "tool_result",
        "call_id": "call-1",
        "output": {"channel": "matrix", "conv": "matrix:room", "id": "event-id"},
        "is_error": false,
    })
}

#[test]
fn trailing_channel_outbound_does_not_become_assistant_prefill() {
    let body = build_test_body(
        &req(vec![user_message("hi"), channel_outbound("already posted")]),
        true,
        None,
    );
    let messages = body["messages"].as_array().expect("messages array");

    assert_eq!(messages.len(), 1);
    assert_ne!(
        messages.last().and_then(|msg| msg["role"].as_str()),
        Some("assistant")
    );
    assert_eq!(messages[0]["role"], "user");
}

#[test]
fn middle_channel_outbound_is_dropped_and_tool_result_sequence_stays_intact() {
    let body = build_test_body(
        &req(vec![
            user_message("send this"),
            assistant_tool_use(),
            tool_result(),
            channel_outbound("posted"),
            user_message("next request"),
        ]),
        true,
        None,
    );
    let messages = body["messages"].as_array().expect("messages array");

    assert_eq!(messages.len(), 4);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"][0]["type"], "tool_use");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "call-1");
    assert_eq!(messages[3]["role"], "user");
}

#[test]
fn channel_send_regression_fixture_maps_without_trailing_assistant() {
    let body = build_test_body(
        &req(vec![
            user_message("send this"),
            assistant_tool_use(),
            tool_result(),
            channel_outbound("posted"),
        ]),
        true,
        None,
    );
    let messages = body["messages"].as_array().expect("messages array");
    let roles: Vec<&str> = messages
        .iter()
        .filter_map(|message| message["role"].as_str())
        .collect();

    assert_eq!(roles, ["user", "assistant", "user"]);
    assert_eq!(
        messages.last().and_then(|msg| msg["role"].as_str()),
        Some("user")
    );
    assert_eq!(
        messages.last().expect("last item should exist")["content"][0]["type"],
        "tool_result"
    );
}
