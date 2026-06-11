use super::*;
use crabgent_core::ToolDef;

fn req() -> LlmRequest {
    LlmRequest {
        model: "claude-sonnet-4-6".into(),
        system_prompt: Some("be terse".into()),
        messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})],
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
    build_body(req, stream, cache_ttl, req.model.as_str()).expect("test build_body")
}

#[test]
fn body_carries_system_and_max_tokens() {
    let body = build_test_body(&req(), true, None);
    assert_eq!(body["model"], "claude-sonnet-4-6");
    assert_eq!(body["max_tokens"], 2048);
    assert_eq!(body["stream"], true);
    assert_eq!(body["system"], "be terse");
    assert_eq!(body["temperature"], 0.5);
    assert!(body.get("tools").is_none());
}

#[test]
fn body_uses_default_max_tokens_when_unset() {
    let mut r = req();
    r.max_tokens = None;
    let body = build_test_body(&r, false, None);
    assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
    assert_eq!(body["stream"], false);
}

#[test]
fn body_omits_temperature_when_none() {
    let mut r = req();
    r.temperature = None;
    let body = build_test_body(&r, false, None);
    assert!(
        body.get("temperature").is_none(),
        "temperature key must be absent when LlmRequest.temperature is None \
         (Anthropic opus-4-7 rejects the field outright)",
    );
}

#[test]
fn body_includes_tools_when_present() {
    let mut r = req();
    r.tools = vec![ToolDef {
        name: "search".into(),
        description: "search the web".into(),
        input_schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
    }];
    let body = build_test_body(&r, true, None);
    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "search");
    assert_eq!(tools[0]["input_schema"]["type"], "object");
}

#[test]
fn assistant_message_transforms_to_content_array() {
    let mut r = req();
    r.messages = vec![
        json!({
            "role": "assistant",
            "text": "let me check",
            "tool_calls": [{"id": "c1", "name": "bash", "args": {"cmd": "ls"}}],
        }),
        json!({"role": "tool_result", "call_id": "c1", "output": "ok"}),
    ];
    let body = build_test_body(&r, true, None);
    let msgs = body["messages"].as_array().expect("messages array");
    assert_eq!(msgs.len(), 2);
    let m = &msgs[0];
    assert_eq!(m["role"], "assistant");
    let content = m["content"].as_array().expect("content array");
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "let me check");
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(content[1]["id"], "c1");
    assert_eq!(content[1]["name"], "bash");
    assert_eq!(content[1]["input"], json!({"cmd": "ls"}));
}

#[test]
fn tool_result_message_transforms_to_user_with_block() {
    let mut r = req();
    r.messages = vec![
        json!({
            "role": "assistant",
            "text": "",
            "tool_calls": [{"id": "c1", "name": "bash", "args": {}}],
        }),
        json!({
            "role": "tool_result",
            "call_id": "c1",
            "output": "ok",
            "is_error": false,
        }),
    ];
    let body = build_test_body(&r, true, None);
    let msgs = body["messages"].as_array().expect("messages array");
    assert_eq!(msgs.len(), 2);
    let m = &msgs[1];
    assert_eq!(m["role"], "user");
    let content = m["content"].as_array().expect("content array");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "tool_result");
    assert_eq!(content[0]["tool_use_id"], "c1");
    assert_eq!(content[0]["content"], "ok");
    assert_eq!(content[0]["is_error"], false);
}

#[test]
fn tool_result_soft_error_flag_is_preserved() {
    let mut r = req();
    r.messages = vec![
        json!({
            "role": "assistant",
            "text": "",
            "tool_calls": [{"id": "c1", "name": "bash", "args": {}}],
        }),
        json!({
            "role": "tool_result",
            "call_id": "c1",
            "output": "validation failed",
            "is_error": true,
        }),
    ];
    let body = build_test_body(&r, true, None);
    let content = body["messages"][1]["content"][0].clone();
    assert_eq!(content["type"], "tool_result");
    assert_eq!(content["tool_use_id"], "c1");
    assert_eq!(content["content"], "validation failed");
    assert_eq!(content["is_error"], true);
}

#[test]
fn tool_result_with_json_output_is_serialized_to_string() {
    let mut r = req();
    r.messages = vec![
        json!({
            "role": "assistant",
            "text": "",
            "tool_calls": [{"id": "c1", "name": "bash", "args": {}}],
        }),
        json!({
            "role": "tool_result",
            "call_id": "c1",
            "output": {"answer": 42},
            "is_error": false,
        }),
    ];
    let body = build_test_body(&r, true, None);
    let content = body["messages"][1]["content"][0].clone();
    assert_eq!(content["content"], "{\"answer\":42}");
}

#[test]
fn system_role_message_is_dropped_from_messages() {
    let mut r = req();
    r.messages = vec![
        json!({"role": "system", "content": "be helpful"}),
        json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
    ];
    let body = build_test_body(&r, true, None);
    let msgs = body["messages"].as_array().expect("messages array");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
}

#[test]
fn user_message_transform_strips_non_anthropic_top_level_fields() {
    let mut r = req();
    r.messages = vec![json!({
        "role": "user",
        "content": [{"type": "image", "mime": "image/png", "data": "cG5n"}],
        "cache_control": {"type": "ephemeral", "ttl": "5m"},
        "timestamp": "2026-05-21T14:32:11Z",
        "source": {"channel": "matrix"}
    })];

    let body = build_test_body(&r, true, None);
    let message = &body["messages"][0];

    // Anthropic returns HTTP 400 for unknown top-level message keys,
    // for example `messages.0.timestamp: Extra inputs are not permitted`.
    let object = message.as_object().expect("wire user message object");
    for key in object.keys() {
        assert!(
            matches!(key.as_str(), "role" | "content" | "cache_control"),
            "unexpected Anthropic message key {key}"
        );
    }
    assert_eq!(message["role"], "user");
    assert_eq!(message["cache_control"]["type"], "ephemeral");
    assert_eq!(message["cache_control"]["ttl"], "5m");
    assert!(message.get("timestamp").is_none());
    assert!(message.get("source").is_none());
    assert_eq!(message["content"][0]["type"], "image");
    assert_eq!(message["content"][0]["source"]["media_type"], "image/png");
}

#[test]
fn assistant_with_only_tool_call_omits_empty_text_block() {
    let mut r = req();
    r.messages = vec![
        json!({
            "role": "assistant",
            "text": "",
            "tool_calls": [{"id": "c1", "name": "bash", "args": {}}],
        }),
        json!({"role": "tool_result", "call_id": "c1", "output": "done"}),
    ];
    let body = build_test_body(&r, true, None);
    let content = body["messages"][0]["content"].as_array().expect("content");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "tool_use");
}

#[test]
fn sanitize_schema_drops_top_level_combinators() {
    let schema = json!({
        "type": "object",
        "allOf": [{"required": ["x"]}],
        "oneOf": [{"required": ["y"]}],
        "anyOf": [{"required": ["z"]}],
        "properties": {"x": {"type": "string"}},
    });
    let sanitized = sanitize_schema(&schema);
    assert_eq!(sanitized["type"], "object");
    assert!(sanitized.get("allOf").is_none());
    assert!(sanitized.get("oneOf").is_none());
    assert!(sanitized.get("anyOf").is_none());
    assert_eq!(sanitized["properties"]["x"]["type"], "string");
}

#[test]
fn sanitize_schema_passes_through_clean_schema() {
    let schema = json!({"type": "object", "properties": {}});
    let sanitized = sanitize_schema(&schema);
    assert_eq!(sanitized, schema);
}

#[test]
fn stop_sequences_included_when_present() {
    let mut r = req();
    r.stop_sequences = vec!["END".into(), "STOP".into()];
    let body = build_test_body(&r, true, None);
    assert_eq!(body["stop_sequences"], json!(["END", "STOP"]));
}

#[test]
fn empty_stop_sequences_omits_field() {
    let body = build_test_body(&req(), true, None);
    assert!(body.get("stop_sequences").is_none());
}

fn cache_req() -> LlmRequest {
    let mut r = req();
    let large_text = "x".repeat(4096);
    r.tools = vec![
        ToolDef {
            name: "first".into(),
            description: "first tool".into(),
            input_schema: json!({"type": "object"}),
        },
        ToolDef {
            name: "last".into(),
            description: "last tool".into(),
            input_schema: json!({"type": "object"}),
        },
    ];
    r.messages = vec![json!({"role": "user", "content": [{"type": "text", "text": large_text}]})];
    r
}

#[test]
fn build_body_sets_cache_control_on_system() {
    let body = build_test_body(&cache_req(), true, Some("5m"));
    assert!(body["system"].is_array());
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["system"][0]["cache_control"]["ttl"], "5m");
}

#[test]
fn build_body_system_wraps_to_array_when_caching_enabled() {
    assert!(build_test_body(&cache_req(), true, Some("5m"))["system"].is_array());
}

#[test]
fn build_body_system_stays_string_when_caching_disabled() {
    let body = build_test_body(&cache_req(), true, None);
    assert_eq!(body["system"], "be terse");
}

#[test]
fn build_body_sets_cache_control_on_last_tool() {
    let body = build_test_body(&cache_req(), true, Some("5m"));
    assert!(body["tools"][0].get("cache_control").is_none());
    assert_eq!(body["tools"][1]["cache_control"]["ttl"], "5m");
}

#[test]
fn build_body_sets_cache_control_on_last_message_block_when_large() {
    let body = build_test_body(&cache_req(), true, Some("5m"));
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
}

#[test]
fn build_body_no_message_breakpoint_when_messages_too_small() {
    let body = build_test_body(&req(), true, Some("5m"));
    let block = &body["messages"][0]["content"][0];
    assert!(block.get("cache_control").is_none());
}

#[test]
fn build_body_no_cache_control_when_ttl_is_none() {
    let body = build_test_body(&cache_req(), true, None);
    let serialized = serde_json::to_string(&body).expect("serialize body");
    assert!(!serialized.contains("cache_control"));
}

#[test]
fn orphan_tool_use_is_dropped() {
    let mut r = req();
    r.messages = vec![
        json!({
            "role": "assistant",
            "text": "thinking",
            "tool_calls": [
                {"id": "call_orphan", "name": "bash", "args": {"cmd": "ls"}},
                {"id": "call_keep", "name": "bash", "args": {"cmd": "pwd"}},
            ],
        }),
        json!({
            "role": "tool_result",
            "call_id": "call_keep",
            "output": "/home",
            "is_error": false,
        }),
    ];
    let body = build_test_body(&r, false, None);
    let msgs = body["messages"].as_array().expect("messages array");
    assert_eq!(msgs.len(), 2);
    let assistant = &msgs[0];
    assert_eq!(assistant["role"], "assistant");
    let content = assistant["content"].as_array().expect("assistant content");
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(content[1]["id"], "call_keep");
    let tool_result = &msgs[1];
    assert_eq!(tool_result["content"][0]["tool_use_id"], "call_keep");
}

#[test]
fn assistant_with_only_orphan_tool_calls_is_dropped() {
    let mut r = req();
    r.messages = vec![
        json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
        json!({
            "role": "assistant",
            "text": "",
            "tool_calls": [{"id": "call_orphan", "name": "bash", "args": {}}],
        }),
    ];
    let body = build_test_body(&r, false, None);
    let msgs = body["messages"].as_array().expect("messages array");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
}

#[test]
fn orphan_tool_result_is_dropped() {
    let mut r = req();
    r.messages = vec![
        json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
        json!({
            "role": "tool_result",
            "call_id": "call_unmatched",
            "output": "stray",
            "is_error": false,
        }),
    ];
    let body = build_test_body(&r, false, None);
    let msgs = body["messages"].as_array().expect("messages array");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
}

#[test]
fn build_body_cache_control_count_never_exceeds_four() {
    let body = build_test_body(&cache_req(), true, Some("5m"));
    let count = serde_json::to_string(&body)
        .expect("serialize body")
        .matches("cache_control")
        .count();
    assert!(count <= 4, "cache_control count {count}");
}

fn req_with_one_tool() -> LlmRequest {
    let mut r = req();
    r.tools = vec![ToolDef {
        name: "search".into(),
        description: "search the web".into(),
        input_schema: json!({"type": "object"}),
    }];
    r
}

#[test]
fn tool_choice_auto_emits_auto_wire_shape() {
    let mut r = req_with_one_tool();
    r.tool_choice = Some(crabgent_core::ToolChoice::Auto);
    let body = build_test_body(&r, true, None);
    assert_eq!(body["tool_choice"], json!({"type": "auto"}));
}

#[test]
fn tool_choice_any_emits_any_wire_shape() {
    let mut r = req_with_one_tool();
    r.tool_choice = Some(crabgent_core::ToolChoice::Any);
    let body = build_test_body(&r, true, None);
    assert_eq!(body["tool_choice"], json!({"type": "any"}));
}

#[test]
fn tool_choice_tool_emits_named_wire_shape() {
    let mut r = req_with_one_tool();
    r.tool_choice = Some(crabgent_core::ToolChoice::Tool("x".into()));
    let body = build_test_body(&r, true, None);
    assert_eq!(body["tool_choice"], json!({"type": "tool", "name": "x"}));
}

#[test]
fn tool_choice_none_emits_none_wire_shape() {
    let mut r = req_with_one_tool();
    r.tool_choice = Some(crabgent_core::ToolChoice::None);
    let body = build_test_body(&r, true, None);
    assert_eq!(body["tool_choice"], json!({"type": "none"}));
}

#[test]
fn tool_choice_omitted_when_no_tools_present() {
    let mut r = req();
    r.tool_choice = Some(crabgent_core::ToolChoice::Tool("x".into()));
    let body = build_test_body(&r, true, None);
    assert!(
        body.get("tool_choice").is_none(),
        "tool_choice must not be emitted without tools",
    );
}

#[test]
fn tool_choice_absent_when_request_field_is_none() {
    let body = build_test_body(&req_with_one_tool(), true, None);
    assert!(
        body.get("tool_choice").is_none(),
        "no tool_choice field when LlmRequest.tool_choice is None",
    );
}

// -- Web search tool tests --
