//! Responses wire-format tool-choice, web-search, and provider-block
//! request assertions. Baseline request/parse tests live in
//! `wire_responses.rs`.

use crabgent_core::{LlmRequest, ToolChoice, ToolDef, WebSearchConfig};
use crabgent_provider_openai::wire::WireFormat;
use crabgent_provider_openai::wire::responses::ResponsesWire;
use serde_json::json;

mod common;
use common::{ctx, req};

fn req_with_web_search(
    messages: Vec<serde_json::Value>,
    allowed: Vec<String>,
    blocked: Vec<String>,
) -> LlmRequest {
    LlmRequest {
        model: "gpt-5.5".into(),
        system_prompt: None,
        messages,
        tools: Vec::new(),
        max_tokens: None,
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig {
            enabled: true,
            max_uses: None,
            allowed_domains: allowed,
            blocked_domains: blocked,
        },
        tool_choice: None,
    }
}

#[test]
fn tools_to_responses_appends_web_search_when_enabled() {
    let request = req_with_web_search(
        vec![json!({"role": "user", "content": "search for rust"})],
        vec![],
        vec![],
    );

    let body = ResponsesWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");

    let tools = body["tools"].as_array().expect("tools array");
    assert!(
        tools.iter().any(|t| t["type"] == "web_search"),
        "web_search tool must appear in tools"
    );
    // No filters when both domain lists are empty.
    let ws_tool = tools
        .iter()
        .find(|t| t["type"] == "web_search")
        .expect("web_search tool");
    assert_eq!(
        ws_tool["external_web_access"], true,
        "web_search must request live external web access"
    );
    assert!(
        ws_tool.get("filters").is_none(),
        "no filters when domains empty"
    );
}

#[test]
fn tools_to_responses_web_search_allowed_domains_filter() {
    let request = req_with_web_search(
        vec![json!({"role": "user", "content": "hi"})],
        vec!["example.com".to_owned()],
        vec![],
    );

    let body = ResponsesWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    let tools = body["tools"].as_array().expect("tools array");
    let ws = tools
        .iter()
        .find(|t| t["type"] == "web_search")
        .expect("web_search");
    assert_eq!(
        ws["filters"]["allowed_domains"][0], "example.com",
        "allowed_domains filter forwarded"
    );
    assert_eq!(
        ws["external_web_access"], true,
        "allowed-domain web_search must still request live access"
    );
    assert!(ws["filters"].get("blocked_domains").is_none());
}

#[test]
fn tools_to_responses_web_search_blocked_domains_filter() {
    let request = req_with_web_search(
        vec![json!({"role": "user", "content": "hi"})],
        vec![],
        vec!["evil.example".to_owned()],
    );

    let body = ResponsesWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    let tools = body["tools"].as_array().expect("tools array");
    let ws = tools
        .iter()
        .find(|t| t["type"] == "web_search")
        .expect("web_search");
    assert_eq!(
        ws["filters"]["blocked_domains"][0], "evil.example",
        "blocked_domains filter forwarded"
    );
    assert_eq!(
        ws["external_web_access"], true,
        "blocked-domain web_search must still request live access"
    );
    assert!(ws["filters"].get("allowed_domains").is_none());
}

#[test]
fn provider_block_openai_is_emitted_verbatim() {
    let block = json!({
        "type": "web_search_call",
        "id": "wsearch_01",
        "status": "completed",
    });
    let request = LlmRequest {
        model: "gpt-5.5".into(),
        system_prompt: None,
        messages: vec![
            json!({"role": "user", "content": "search"}),
            json!({"role": "provider_block", "provider": "openai", "block": block}),
        ],
        tools: Vec::new(),
        max_tokens: None,
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    };

    let body = ResponsesWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    let input = body["input"].as_array().expect("input array");
    // The provider_block should appear verbatim in input
    assert!(
        input.iter().any(|item| item["type"] == "web_search_call"),
        "openai ProviderBlock emitted verbatim into input"
    );
}

#[test]
fn provider_block_non_openai_is_skipped() {
    let request = LlmRequest {
        model: "gpt-5.5".into(),
        system_prompt: None,
        messages: vec![
            json!({"role": "user", "content": "search"}),
            json!({
                "role": "provider_block",
                "provider": "anthropic",
                "block": {"type": "web_search_tool_result", "id": "x"}
            }),
        ],
        tools: Vec::new(),
        max_tokens: None,
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    };

    let body = ResponsesWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    let input = body["input"].as_array().expect("input array");
    // Only the user message should appear; anthropic block is dropped
    assert_eq!(input.len(), 1, "anthropic ProviderBlock must be skipped");
    assert_eq!(input[0]["role"], "user");
}

fn req_with_tool(choice: ToolChoice) -> LlmRequest {
    let mut request = req(vec![json!({
        "role": "user",
        "content": [{"type": "text", "text": "search"}],
    })]);
    request.tools = vec![ToolDef {
        name: "search".to_owned(),
        description: "search docs".to_owned(),
        input_schema: json!({"type": "object"}),
    }];
    request.tool_choice = Some(choice);
    request
}

#[test]
fn tool_choice_auto_maps_to_string() {
    let body = ResponsesWire
        .build_body(&req_with_tool(ToolChoice::Auto), &ctx(), false)
        .expect("build_body");
    assert_eq!(body["tool_choice"], "auto");
}

#[test]
fn tool_choice_any_maps_to_required() {
    let body = ResponsesWire
        .build_body(&req_with_tool(ToolChoice::Any), &ctx(), false)
        .expect("build_body");
    assert_eq!(body["tool_choice"], "required");
}

#[test]
fn tool_choice_none_maps_to_string() {
    let body = ResponsesWire
        .build_body(&req_with_tool(ToolChoice::None), &ctx(), false)
        .expect("build_body");
    assert_eq!(body["tool_choice"], "none");
}

#[test]
fn tool_choice_tool_maps_to_function_object() {
    let body = ResponsesWire
        .build_body(
            &req_with_tool(ToolChoice::Tool("search".to_owned())),
            &ctx(),
            false,
        )
        .expect("build_body");
    assert_eq!(body["tool_choice"]["type"], "function");
    assert_eq!(body["tool_choice"]["name"], "search");
}

#[test]
fn tool_choice_omitted_when_no_tools() {
    // tool_choice is set, but no tools present: the field stays omitted.
    let mut request = req(vec![json!({
        "role": "user",
        "content": [{"type": "text", "text": "hi"}],
    })]);
    request.tool_choice = Some(ToolChoice::Tool("search".to_owned()));
    let body = ResponsesWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    assert!(
        body.get("tool_choice").is_none(),
        "tool_choice must be omitted when no tools are present"
    );
}

#[test]
fn tool_choice_omitted_when_unset_with_tools() {
    // No tool_choice on the request: the dropped server-default "auto"
    // means the field stays absent even when tools are present.
    let mut request = req(vec![json!({
        "role": "user",
        "content": [{"type": "text", "text": "search"}],
    })]);
    request.tools = vec![ToolDef {
        name: "search".to_owned(),
        description: "search docs".to_owned(),
        input_schema: json!({"type": "object"}),
    }];
    let body = ResponsesWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    assert!(
        body.get("tool_choice").is_none(),
        "tool_choice must be omitted when unset (server default is auto)"
    );
}
