use crabgent_core::{LlmRequest, ToolChoice, ToolDef, WebSearchConfig};
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
    let body = ChatCompletionsWire
        .build_body(&req_with_tool(ToolChoice::Auto), &ctx(), false)
        .expect("build_body");
    assert_eq!(body["tool_choice"], "auto");
}

#[test]
fn tool_choice_any_maps_to_required() {
    let body = ChatCompletionsWire
        .build_body(&req_with_tool(ToolChoice::Any), &ctx(), false)
        .expect("build_body");
    assert_eq!(body["tool_choice"], "required");
}

#[test]
fn tool_choice_none_maps_to_string() {
    let body = ChatCompletionsWire
        .build_body(&req_with_tool(ToolChoice::None), &ctx(), false)
        .expect("build_body");
    assert_eq!(body["tool_choice"], "none");
}

#[test]
fn tool_choice_tool_maps_to_function_object() {
    let body = ChatCompletionsWire
        .build_body(
            &req_with_tool(ToolChoice::Tool("search".to_owned())),
            &ctx(),
            false,
        )
        .expect("build_body");
    assert_eq!(body["tool_choice"]["type"], "function");
    assert_eq!(body["tool_choice"]["function"]["name"], "search");
}

#[test]
fn tool_choice_omitted_when_no_tools() {
    // tool_choice is set, but no tools present: the field stays omitted.
    let mut request = req(vec![json!({
        "role": "user",
        "content": [{"type": "text", "text": "hi"}],
    })]);
    request.tool_choice = Some(ToolChoice::Tool("search".to_owned()));
    let body = ChatCompletionsWire
        .build_body(&request, &ctx(), false)
        .expect("build_body");
    assert!(
        body.get("tool_choice").is_none(),
        "tool_choice must be omitted when no tools are present"
    );
}
