use crabgent_core::{LlmRequest, StopReason, ToolDef, WebSearchConfig};
use crabgent_provider_openai::wire::WireFormat;
use crabgent_provider_openai::wire::chat_completions::ChatCompletionsWire;
use serde_json::{Value, json};

fn req(messages: Vec<Value>) -> LlmRequest {
    LlmRequest {
        model: "gpt-5.5".into(),
        system_prompt: None,
        messages,
        tools: Vec::new(),
        max_tokens: Some(128),
        temperature: Some(0.2),
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    }
}

fn ctx() -> crabgent_core::RunCtx {
    crabgent_core::RunCtx::new(
        crabgent_core::RunId::new(),
        crabgent_core::Subject::new("test"),
    )
}

#[test]
fn transform_user_image_block_chat_completions() {
    let request = req(vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "describe"},
            {"type": "image", "mime": "image/png", "data": "iVBORw0K"}
        ],
    })]);

    let body = ChatCompletionsWire
        .build_body(
            &request,
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            false,
        )
        .expect("build_body");
    let image = &body["messages"][0]["content"][1];
    assert_eq!(image["type"], "image_url");
    assert!(
        image["image_url"]["url"]
            .as_str()
            .expect("image url")
            .starts_with("data:image/png;base64,")
    );
    assert_eq!(image["image_url"]["detail"], "auto");
}

#[test]
fn transform_user_audio_block_chat_completions() {
    let request = req(vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "what is said?"},
            {"type": "audio", "mime": "audio/wav", "data": "UklGRiQ=", "filename": "note.wav"}
        ],
    })]);

    let body = ChatCompletionsWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    let audio = &body["messages"][0]["content"][1];
    assert_eq!(audio["type"], "input_audio");
    assert_eq!(audio["input_audio"]["data"], "UklGRiQ=");
    assert_eq!(audio["input_audio"]["format"], "wav");
}

#[test]
fn audio_block_mpeg_maps_to_mp3_format() {
    let request = req(vec![json!({
        "role": "user",
        "content": [{"type": "audio", "mime": "audio/mpeg", "data": "SUQz"}],
    })]);
    let body = ChatCompletionsWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    assert_eq!(
        body["messages"][0]["content"][0]["input_audio"]["format"],
        "mp3"
    );
}

#[test]
fn unsupported_audio_format_is_dropped() {
    let request = req(vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "keep me"},
            {"type": "audio", "mime": "audio/ogg", "data": "T2dn"}
        ],
    })]);
    let body = ChatCompletionsWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    let content = body["messages"][0]["content"]
        .as_array()
        .expect("content array");
    // The unsupported audio block is dropped; only the text survives.
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
}

#[test]
fn build_body_includes_tools() {
    let mut request = req(vec![json!({
        "role": "user",
        "content": [{"type": "text", "text": "search"}],
    })]);
    request.tools = vec![ToolDef {
        name: "search".to_owned(),
        description: "search docs".to_owned(),
        input_schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
    }];

    let body = ChatCompletionsWire
        .build_body(
            &request,
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            true,
        )
        .expect("build_body");
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "search");
    assert_eq!(
        body["tools"][0]["function"]["parameters"]["properties"]["q"]["type"],
        "string"
    );
}

#[test]
fn build_body_handles_system_prompt() {
    let mut request = req(vec![json!({
        "role": "user",
        "content": [{"type": "text", "text": "hi"}],
    })]);
    request.system_prompt = Some("be precise".to_owned());

    let body = ChatCompletionsWire
        .build_body(
            &request,
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            false,
        )
        .expect("build_body");
    assert_eq!(
        body["messages"][0],
        json!({"role": "system", "content": "be precise"})
    );
    assert_eq!(body["messages"][1]["role"], "user");
}

#[test]
fn build_body_serializes_model_field() {
    let body = ChatCompletionsWire
        .build_body(
            &req(Vec::new()),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            true,
        )
        .expect("build_body");

    assert_eq!(body["model"], "gpt-5.5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_completion_tokens"], 128);
    assert!(
        body.get("max_tokens").is_none(),
        "legacy max_tokens must not leak"
    );
    // gpt-5.x is a reasoning model: explicit temperature must be
    // dropped because the API rejects anything other than the default.
    assert!(
        body.get("temperature").is_none(),
        "temperature must not be sent for reasoning models"
    );
}

#[test]
fn build_body_keeps_temperature_for_legacy_models() {
    let mut request = req(Vec::new());
    request.model = "gpt-4o".into();
    let body = ChatCompletionsWire
        .build_body(
            &request,
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            true,
        )
        .expect("build_body");
    let temperature = body["temperature"].as_f64().expect("temperature number");
    assert!((temperature - 0.2).abs() < 0.000_001);
}

#[test]
fn build_body_drops_orphan_tool_calls() {
    let request = req(vec![
        json!({"role": "user", "content": [{"type": "text", "text": "do it"}]}),
        json!({
            "role": "assistant",
            "text": "calling",
            "tool_calls": [
                {"id": "call_keep", "name": "search", "args": {}},
                {"id": "call_drop", "name": "search", "args": {}},
            ],
        }),
        json!({"role": "tool_result", "call_id": "call_keep", "output": {"ok": true}}),
    ]);
    let body = ChatCompletionsWire
        .build_body(
            &request,
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            false,
        )
        .expect("build_body");
    let messages = body["messages"].as_array().expect("messages array");
    let assistant = messages
        .iter()
        .find(|m| m["role"] == "assistant" && m.get("tool_calls").is_some())
        .expect("assistant message with tool_calls");
    let surviving = assistant["tool_calls"]
        .as_array()
        .expect("tool_calls array");
    assert_eq!(surviving.len(), 1);
    assert_eq!(surviving[0]["id"], "call_keep");
}

#[test]
fn build_body_drops_orphan_tool_reply() {
    let request = req(vec![
        json!({"role": "user", "content": [{"type": "text", "text": "do it"}]}),
        json!({
            "role": "assistant",
            "text": "calling",
            "tool_calls": [{"id": "call_keep", "name": "search", "args": {}}],
        }),
        json!({"role": "tool_result", "call_id": "call_keep", "output": {"ok": true}}),
        json!({"role": "tool_result", "call_id": "ghost_id", "output": {"ghost": true}}),
    ]);
    let body = ChatCompletionsWire
        .build_body(
            &request,
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            false,
        )
        .expect("build_body");
    let messages = body["messages"].as_array().expect("messages array");
    let tool_messages: Vec<&Value> = messages.iter().filter(|m| m["role"] == "tool").collect();
    assert_eq!(tool_messages.len(), 1);
    assert_eq!(tool_messages[0]["tool_call_id"], "call_keep");
}

#[test]
fn build_body_drops_fully_orphaned_assistant_message() {
    let request = req(vec![
        json!({"role": "user", "content": [{"type": "text", "text": "do it"}]}),
        json!({
            "role": "assistant",
            "text": "",
            "tool_calls": [{"id": "call_drop", "name": "search", "args": {}}],
        }),
    ]);
    let body = ChatCompletionsWire
        .build_body(
            &request,
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            false,
        )
        .expect("build_body");
    let messages = body["messages"].as_array().expect("messages array");
    assert!(messages.iter().all(|m| m.get("tool_calls").is_none()));
}

#[test]
fn build_body_transforms_message_variants() {
    let request = req(vec![
        json!({"role": "system", "text": "follow local policy"}),
        json!({"role": "user", "content": "plain text"}),
        json!({
            "role": "assistant",
            "text": "using tool",
            "tool_calls": [{
                "id": "call_1",
                "name": "search",
                "args": {"q": "rust"}
            }]
        }),
        json!({"role": "tool_result", "call_id": "call_1", "output": {"answer": 42}}),
        json!({"role": "channel_outbound", "body": "sent to channel"}),
        json!({"role": "custom", "content": "kept"}),
        json!({"content": "missing role"}),
    ]);

    let body = ChatCompletionsWire
        .build_body(
            &request,
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            false,
        )
        .expect("build_body");
    let messages = body["messages"].as_array().expect("messages array");

    assert_eq!(
        messages[0],
        json!({"role": "system", "content": "follow local policy"})
    );
    assert_eq!(
        messages[1],
        json!({"role": "user", "content": "plain text"})
    );
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["tool_calls"][0]["function"]["name"], "search");
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["arguments"],
        "{\"q\":\"rust\"}"
    );
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["content"], "{\"answer\":42}");
    assert_eq!(
        messages[4],
        json!({"role": "assistant", "content": "sent to channel"})
    );
    assert_eq!(messages[5], json!({"role": "custom", "content": "kept"}));
    assert_eq!(messages.len(), 6);
}

#[test]
fn parse_response_maps_standard_shape() {
    let body = json!({
        "model": "gpt-5.5",
        "choices": [{
            "message": {"content": "hello"},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 5,
            "completion_tokens": 3,
            "prompt_tokens_details": {"cached_tokens": 2}
        }
    });

    let response = ChatCompletionsWire
        .parse_response(body)
        .expect("standard chat completion response");
    assert_eq!(response.text, "hello");
    assert_eq!(response.usage.input_tokens, 5);
    assert_eq!(response.usage.output_tokens, 3);
    assert_eq!(response.usage.cache_read_tokens, 2);
}

#[test]
fn parse_response_maps_tool_calls_and_finish_reasons() {
    let body = json!({
        "model": "gpt-5.5",
        "choices": [{
            "message": {
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "function": {"name": "search", "arguments": "{\"q\":\"rust\"}"}
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });

    let response = ChatCompletionsWire
        .parse_response(body)
        .expect("tool call response");
    assert_eq!(response.text, "");
    assert_eq!(response.tool_calls[0].name, "search");
    assert_eq!(response.tool_calls[0].args, json!({"q": "rust"}));
    assert!(matches!(response.stop_reason, StopReason::ToolUse));

    for (finish_reason, expected) in [
        ("length", StopReason::MaxTokens),
        ("unknown", StopReason::Other),
    ] {
        let body = json!({
            "model": "gpt-5.5",
            "choices": [{
                "message": {"content": "done"},
                "finish_reason": finish_reason
            }]
        });
        let response = ChatCompletionsWire
            .parse_response(body)
            .expect("finish reason response");
        assert_eq!(response.stop_reason, expected);
    }
}

#[test]
fn parse_response_rejects_empty_choices_and_bad_tool_arguments() {
    let empty = json!({"model": "gpt-5.5", "choices": []});
    ChatCompletionsWire
        .parse_response(empty)
        .expect_err("expected error");

    let bad_arguments = json!({
        "model": "gpt-5.5",
        "choices": [{
            "message": {
                "tool_calls": [{
                    "id": "call_1",
                    "function": {"name": "search", "arguments": "{not-json"}
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    ChatCompletionsWire
        .parse_response(bad_arguments)
        .expect_err("expected error");
}

#[test]
fn build_body_rejects_web_search_enabled() {
    let mut request = req(vec![json!({"role": "user", "content": "search for rust"})]);
    request.web_search = crabgent_core::types::WebSearchConfig {
        enabled: true,
        max_uses: None,
        allowed_domains: vec![],
        blocked_domains: vec![],
    };

    let result = ChatCompletionsWire.build_body(
        &request,
        &crabgent_core::RunCtx::new(
            crabgent_core::RunId::new(),
            crabgent_core::Subject::new("test"),
        ),
        false,
    );

    assert!(
        result.is_err(),
        "build_body must fail when web_search.enabled on ChatCompletions wire"
    );
    let err = result.expect_err("expected error");
    let msg = err.to_string();
    assert!(
        msg.contains("Chat Completions"),
        "error message must reference Chat Completions wire: {msg}"
    );
}
