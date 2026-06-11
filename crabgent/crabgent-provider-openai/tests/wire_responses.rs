//! Responses wire-format request build + response parse assertions.
//! Tool-choice and web-search wire tests live in `wire_responses_tools.rs`;
//! SSE/streaming assertions live in `sse_responses.rs`.

use crabgent_core::StopReason;
use crabgent_provider_openai::wire::WireFormat;
use crabgent_provider_openai::wire::responses::ResponsesWire;
use serde_json::json;

mod common;
use common::{ctx, req};

#[test]
fn transform_user_image_block_responses() {
    let request = req(vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "describe"},
            {"type": "image", "mime": "image/png", "data": "iVBORw0K"}
        ],
    })]);

    let body = ResponsesWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    let image = &body["input"][0]["content"][1];
    assert_eq!(image["type"], "input_image");
    assert!(
        image["image_url"]
            .as_str()
            .expect("image url")
            .starts_with("data:image/png;base64,")
    );
    assert!(image.get("detail").is_none());
}

#[test]
fn build_body_includes_instructions() {
    let mut request = req(vec![json!({
        "role": "user",
        "content": [{"type": "text", "text": "hi"}],
    })]);
    request.system_prompt = Some("be precise".to_owned());

    let body = ResponsesWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    assert_eq!(body["instructions"], "be precise");
    assert_eq!(body["store"], false);
}

#[test]
fn build_body_includes_tools() {
    let mut request = req(vec![json!({
        "role": "user",
        "content": [{"type": "text", "text": "search"}],
    })]);
    request.tools = vec![crabgent_core::ToolDef {
        name: "search".to_owned(),
        description: "search docs".to_owned(),
        input_schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
    }];

    let body = ResponsesWire
        .build_body(&request, &ctx(), true)
        .expect("build_body");
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "search");
    assert_eq!(
        body["tools"][0]["parameters"]["properties"]["q"]["type"],
        "string"
    );
}

#[test]
fn build_body_uses_input_array() {
    let body = ResponsesWire
        .build_body(
            &req(vec![json!({
                "role": "user",
                "content": [{"type": "text", "text": "hi"}],
            })]),
            &ctx(),
            true,
        )
        .expect("build_body");

    assert!(body["input"].is_array());
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert!(body.get("messages").is_none());
    assert_eq!(body["stream"], true);
}

#[test]
fn build_body_transforms_message_variants() {
    let request = req(vec![
        json!({"role": "system", "content": "ignored in input"}),
        json!({"role": "user", "content": "plain text"}),
        json!({
            "role": "assistant",
            "text": "draft",
            "tool_calls": [{"id": "call_1", "name": "search", "args": {"q": "x"}}],
        }),
        json!({"role": "tool_result", "call_id": "call_1", "output": {"answer": 42}}),
        json!({"role": "channel_outbound", "body": "sent to channel"}),
        json!({"role": "custom", "content": "kept"}),
        json!({"content": "missing role"}),
    ]);

    let body = ResponsesWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    let input = body["input"].as_array().expect("input array");

    // system/channel_outbound dropped from input (instructions field
    // carries the system prompt; channel_outbound is audit-only and
    // the function_call + function_call_output pair already represents
    // the delivery).
    assert_eq!(input[0], json!({"role": "user", "content": "plain text"}));
    assert_eq!(input[1]["type"], "message");
    assert_eq!(input[1]["role"], "assistant");
    assert_eq!(input[1]["content"][0]["text"], "draft");
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "call_1");
    assert_eq!(input[2]["name"], "search");
    assert_eq!(
        input[3],
        json!({
            "type": "function_call_output",
            "call_id": "call_1",
            "output": "{\"answer\":42}"
        })
    );
    assert_eq!(input[4], json!({"role": "custom", "content": "kept"}));
    assert_eq!(input[5], json!({"content": "missing role"}));
    assert_eq!(input.len(), 6);
}

#[test]
fn parse_response_maps_standard_shape() {
    let body = json!({
        "model": "gpt-5.3-codex",
        "status": "completed",
        "output": [
            {"type": "message", "content": [{"type": "output_text", "text": "hello"}]},
            {"type": "function_call", "call_id": "call_1", "name": "search", "arguments": "{\"q\":\"rust\"}"}
        ],
        "usage": {
            "input_tokens": 5,
            "output_tokens": 3,
            "input_tokens_details": {"cached_tokens": 2}
        }
    });

    let response = ResponsesWire
        .parse_response(body)
        .expect("standard responses response");
    assert_eq!(response.text, "hello");
    assert_eq!(response.tool_calls[0].id, "call_1");
    assert_eq!(response.tool_calls[0].args, json!({"q": "rust"}));
    assert_eq!(response.usage.cache_read_tokens, 2);
}

#[test]
fn parse_response_maps_statuses_defaults_and_other_items() {
    let body = json!({
        "status": "requires_action",
        "output": [
            {"type": "message", "content": [
                {"type": "output_text", "text": "hel"},
                {"type": "refusal", "text": "ignored"},
                {"type": "output_text", "text": "lo"}
            ]},
            {"type": "custom_ignored"}
        ]
    });

    let response = ResponsesWire
        .parse_response(body)
        .expect("requires action response");
    assert_eq!(response.text, "hello");
    assert_eq!(response.model.to_string(), "");
    assert!(matches!(response.stop_reason, StopReason::ToolUse));

    for (status, expected) in [
        ("incomplete", StopReason::MaxTokens),
        ("mystery", StopReason::Other),
    ] {
        let body = json!({
            "model": "gpt-5.3-codex",
            "status": status,
            "output": []
        });
        let response = ResponsesWire.parse_response(body).expect("status response");
        assert_eq!(response.stop_reason, expected);
    }
}

#[test]
fn parse_response_rejects_bad_function_arguments() {
    let body = json!({
        "model": "gpt-5.3-codex",
        "status": "requires_action",
        "output": [{
            "type": "function_call",
            "call_id": "call_1",
            "name": "search",
            "arguments": "{not-json"
        }]
    });

    ResponsesWire
        .parse_response(body)
        .expect_err("expected error");
}
