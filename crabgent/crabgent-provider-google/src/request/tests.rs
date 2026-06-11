use crabgent_core::{ToolChoice, ToolDef, types::WebSearchConfig};

use super::*;

#[test]
fn schema_type_union_with_null_becomes_nullable() {
    let schema = json!({"type": ["string", "null"]});

    assert_eq!(
        sanitize_schema_for_gemini(&schema),
        json!({"type": "string", "nullable": true})
    );

    let reversed = json!({"type": ["null", "string"]});
    assert_eq!(
        sanitize_schema_for_gemini(&reversed),
        json!({"type": "string", "nullable": true})
    );
}

#[test]
fn schema_drops_additional_properties_false() {
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {"q": {"type": "string"}}
    });

    assert_eq!(
        sanitize_schema_for_gemini(&schema),
        json!({
            "type": "object",
            "properties": {"q": {"type": "string"}}
        })
    );
}

#[test]
fn schema_sanitizes_nested_properties_recursively() {
    let schema = json!({
        "type": "object",
        "properties": {
            "foo": {
                "type": "object",
                "properties": {
                    "bar": {"type": ["null", "string"]}
                }
            }
        }
    });

    assert_eq!(
        sanitize_schema_for_gemini(&schema)["properties"]["foo"]["properties"]["bar"],
        json!({"type": "string", "nullable": true})
    );
}

#[test]
fn schema_without_incompatible_fields_is_unchanged() {
    let schema = json!({
        "type": "object",
        "properties": {
            "q": {"type": "string", "description": "query", "format": "uuid"},
            "limit": {"type": "integer", "minimum": 1}
        },
        "required": ["q"],
        "additionalProperties": true
    });

    assert_eq!(sanitize_schema_for_gemini(&schema), schema);
}

#[test]
fn schema_unwraps_single_non_null_one_of_branch() {
    let schema = json!({
        "description": "optional id",
        "oneOf": [
            {"type": "null"},
            {"type": "string", "format": "uuid"}
        ]
    });

    assert_eq!(
        sanitize_schema_for_gemini(&schema),
        json!({"type": "string", "format": "uuid", "description": "optional id"})
    );
}

#[test]
fn tools_to_google_sanitizes_alice_style_tool_schema() {
    let req = LlmRequest {
        model: "gemini-2.5-flash".into(),
        system_prompt: None,
        messages: vec![json!({"role": "user", "content": "remember this"})],
        tools: vec![ToolDef {
            name: "memory".to_owned(),
            description: "memory tool".to_owned(),
            input_schema: json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "op": {"type": "string"},
                    "scope": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "owner": {"type": ["string", "null"], "format": "uuid"},
                            "conversation_id": {"type": ["null", "string"]},
                            "kind": {"type": ["string", "null"], "format": "uri"}
                        }
                    },
                    "model": {
                        "type": "object",
                        "properties": {
                            "provider": {"type": ["string", "null"]},
                            "id": {"type": ["null", "string"]}
                        }
                    }
                }
            }),
        }],
        max_tokens: None,
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    };

    let tools = tools_to_google(&req);
    assert!(!tools.iter().any(contains_array_typed_type));
    let parameters = tools
        .first()
        .and_then(|tool| tool.pointer("/functionDeclarations/0/parameters"))
        .expect("function parameters");
    assert_eq!(
        parameters.pointer("/properties/scope/properties/owner"),
        Some(&json!({"type": "string", "format": "uuid", "nullable": true}))
    );
    assert!(parameters.get("additionalProperties").is_none());
    assert!(
        parameters
            .pointer("/properties/scope")
            .expect("scope schema")
            .get("additionalProperties")
            .is_none()
    );
    assert!(
        parameters
            .pointer("/properties/scope/properties/kind")
            .expect("kind schema")
            .get("format")
            .is_none()
    );
}

#[test]
fn body_for_function_tools_only_omits_tool_config() {
    let req = LlmRequest {
        tools: vec![test_tool("lookup")],
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert!(body.get("toolConfig").is_none());
}

#[test]
fn body_for_web_search_only_omits_tool_config() {
    let req = LlmRequest {
        web_search: web_search_enabled(),
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert!(body.get("toolConfig").is_none());
}

#[test]
fn body_for_function_tools_and_web_search_enables_server_side_tool_invocations() {
    let req = LlmRequest {
        tools: vec![test_tool("lookup")],
        web_search: web_search_enabled(),
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert_eq!(
        body.get("toolConfig"),
        Some(&json!({"includeServerSideToolInvocations": true}))
    );
}

#[test]
fn body_for_400_reproducer_includes_server_side_tool_invocation_opt_in() {
    let req = LlmRequest {
        model: "gemini-3.5-flash".into(),
        tools: vec![
            test_tool("models"),
            ToolDef {
                name: "task".to_owned(),
                description: "delegate a task".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "prompt": {"type": "string"},
                        "model": {
                            "type": "object",
                            "properties": {
                                "provider": {"type": ["string", "null"]},
                                "id": {"type": ["null", "string"]}
                            }
                        }
                    }
                }),
            },
        ],
        web_search: web_search_enabled(),
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert_eq!(
        body.get("toolConfig"),
        Some(&json!({"includeServerSideToolInvocations": true}))
    );
    let function_declarations = body
        .pointer("/tools/0/functionDeclarations")
        .expect("function declarations");
    assert!(!contains_array_typed_type(function_declarations));
}

#[test]
fn body_for_thinking_model_maps_reasoning_effort_to_thinking_config() {
    let req = LlmRequest {
        reasoning_effort: Some(ReasoningEffort::High),
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert_eq!(
        body.pointer("/generationConfig/thinkingConfig"),
        Some(&json!({"thinkingBudget": 24000, "includeThoughts": true}))
    );
}

#[test]
fn body_for_non_thinking_model_omits_thinking_config() {
    let req = LlmRequest {
        model: "gemini-2.0-flash".into(),
        reasoning_effort: Some(ReasoningEffort::High),
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert!(body.get("generationConfig").is_none());
}

#[test]
fn body_maps_audio_blocks_to_inline_data() {
    let req = LlmRequest {
        messages: vec![json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "transcribe"},
                {"type": "audio", "mime": "audio/wav", "data": "base64-audio", "filename": "clip.wav"}
            ]
        })],
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert_eq!(
        body.pointer("/contents/0/parts/1/inlineData"),
        Some(&json!({"mimeType": "audio/wav", "data": "base64-audio"}))
    );
}

#[test]
fn cached_content_body_contains_stable_prefix() {
    let req = LlmRequest {
        tools: vec![test_tool("lookup")],
        web_search: web_search_enabled(),
        ..base_request()
    };

    let body = build_cached_content_body(&req).expect("cacheable body");

    assert_eq!(body.get("model"), Some(&json!("models/gemini-2.5-flash")));
    assert_eq!(body.get("ttl"), Some(&json!("3600s")));
    assert_eq!(
        body.pointer("/systemInstruction/parts/0/text"),
        Some(&json!("system"))
    );
    assert!(body.get("contents").is_none());
    assert!(body.pointer("/tools/0/functionDeclarations").is_some());
    assert!(body.pointer("/tools/1/google_search").is_some());
    assert_eq!(
        body.get("toolConfig"),
        Some(&json!({"includeServerSideToolInvocations": true}))
    );
}

#[test]
fn generate_body_with_cached_content_omits_cached_prefix_fields() {
    let req = LlmRequest {
        tools: vec![test_tool("lookup")],
        web_search: web_search_enabled(),
        ..base_request()
    };

    let body = build_generate_content_body_with_cache(&req, "cachedContents/abc");

    assert_eq!(
        body.get("cachedContent"),
        Some(&json!("cachedContents/abc"))
    );
    assert!(body.get("systemInstruction").is_none());
    assert!(body.get("tools").is_none());
    assert!(body.get("toolConfig").is_none());
    assert_eq!(
        body.pointer("/contents/0/parts/0/text"),
        Some(&json!("hello"))
    );
}

#[test]
fn tool_choice_auto_maps_to_function_calling_config_auto() {
    let req = LlmRequest {
        tools: vec![test_tool("lookup")],
        tool_choice: Some(ToolChoice::Auto),
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert_eq!(
        body.pointer("/toolConfig/functionCallingConfig"),
        Some(&json!({"mode": "AUTO"}))
    );
}

#[test]
fn tool_choice_any_maps_to_function_calling_config_any() {
    let req = LlmRequest {
        tools: vec![test_tool("lookup")],
        tool_choice: Some(ToolChoice::Any),
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert_eq!(
        body.pointer("/toolConfig/functionCallingConfig"),
        Some(&json!({"mode": "ANY"}))
    );
}

#[test]
fn tool_choice_none_maps_to_function_calling_config_none() {
    let req = LlmRequest {
        tools: vec![test_tool("lookup")],
        tool_choice: Some(ToolChoice::None),
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert_eq!(
        body.pointer("/toolConfig/functionCallingConfig"),
        Some(&json!({"mode": "NONE"}))
    );
}

#[test]
fn tool_choice_named_tool_maps_to_allowed_function_names() {
    let req = LlmRequest {
        tools: vec![test_tool("lookup")],
        tool_choice: Some(ToolChoice::Tool("lookup".to_owned())),
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert_eq!(
        body.pointer("/toolConfig/functionCallingConfig"),
        Some(&json!({"mode": "ANY", "allowedFunctionNames": ["lookup"]}))
    );
}

#[test]
fn tool_choice_without_function_tools_omits_function_calling_config() {
    let req = LlmRequest {
        tools: Vec::new(),
        tool_choice: Some(ToolChoice::Any),
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert!(body.pointer("/toolConfig/functionCallingConfig").is_none());
}

#[test]
fn tool_choice_with_cached_content_omits_function_calling_config() {
    let req = LlmRequest {
        tools: vec![test_tool("lookup")],
        tool_choice: Some(ToolChoice::Any),
        ..base_request()
    };

    let body = build_generate_content_body_with_cache(&req, "cachedContents/abc");

    assert!(body.pointer("/toolConfig/functionCallingConfig").is_none());
}

#[test]
fn tool_choice_merges_with_server_side_tool_invocations() {
    let req = LlmRequest {
        tools: vec![test_tool("lookup")],
        web_search: web_search_enabled(),
        tool_choice: Some(ToolChoice::Any),
        ..base_request()
    };

    let body = build_generate_content_body(&req);

    assert_eq!(
        body.pointer("/toolConfig"),
        Some(&json!({
            "includeServerSideToolInvocations": true,
            "functionCallingConfig": {"mode": "ANY"}
        }))
    );
}

fn base_request() -> LlmRequest {
    LlmRequest {
        model: "gemini-2.5-flash".into(),
        system_prompt: Some("system".to_owned()),
        messages: vec![json!({"role": "user", "content": "hello"})],
        tools: Vec::new(),
        max_tokens: None,
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    }
}

fn test_tool(name: &str) -> ToolDef {
    ToolDef {
        name: name.to_owned(),
        description: "test tool".to_owned(),
        input_schema: json!({"type": "object"}),
    }
}

fn web_search_enabled() -> WebSearchConfig {
    WebSearchConfig {
        enabled: true,
        ..WebSearchConfig::default()
    }
}

fn contains_array_typed_type(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_array_typed_type),
        Value::Object(object) => {
            object.get("type").is_some_and(Value::is_array)
                || object.values().any(contains_array_typed_type)
        }
        _ => false,
    }
}
