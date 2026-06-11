use super::*;
use serde_json::{Value, json};

fn build_test_body(req: &LlmRequest, stream: bool, cache_ttl: Option<&str>) -> Value {
    build_body(req, stream, cache_ttl, req.model.as_str()).expect("build_body")
}

fn req() -> LlmRequest {
    LlmRequest {
        model: "claude-sonnet-4-6".into(),
        system_prompt: None,
        messages: vec![],
        tools: vec![],
        max_tokens: Some(512),
        temperature: None,
        stop_sequences: vec![],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    }
}

fn ws_req(enabled: bool) -> LlmRequest {
    LlmRequest {
        model: "claude-sonnet-4-6".into(),
        system_prompt: None,
        messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})],
        tools: vec![],
        max_tokens: Some(512),
        temperature: None,
        stop_sequences: vec![],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig {
            enabled,
            max_uses: None,
            allowed_domains: vec![],
            blocked_domains: vec![],
        },
        tool_choice: None,
    }
}

#[test]
fn web_search_disabled_omits_server_tool() {
    let body = build_test_body(&ws_req(false), false, None);
    assert!(body.get("tools").is_none());
}

#[test]
fn web_search_enabled_appends_server_tool_entry() {
    let body = build_test_body(&ws_req(true), false, None);
    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], "web_search_20250305");
    assert_eq!(tools[0]["name"], "web_search");
}

#[test]
fn web_search_enabled_with_user_tools_appends_after_user_tools() {
    let mut r = ws_req(true);
    r.tools = vec![crabgent_core::ToolDef {
        name: "bash".into(),
        description: "run bash".into(),
        input_schema: json!({"type": "object"}),
    }];
    let body = build_test_body(&r, false, None);
    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], "bash");
    assert_eq!(tools[1]["type"], "web_search_20250305");
}

#[test]
fn web_search_max_uses_included_when_set() {
    let r = LlmRequest {
        model: "claude-sonnet-4-6".into(),
        system_prompt: None,
        messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})],
        tools: vec![],
        max_tokens: Some(512),
        temperature: None,
        stop_sequences: vec![],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig {
            enabled: true,
            max_uses: Some(3),
            allowed_domains: vec![],
            blocked_domains: vec![],
        },
        tool_choice: None,
    };
    let body = build_test_body(&r, false, None);
    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools[0]["max_uses"], 3);
}

#[test]
fn web_search_allowed_domains_included() {
    let r = LlmRequest {
        model: "claude-sonnet-4-6".into(),
        system_prompt: None,
        messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})],
        tools: vec![],
        max_tokens: Some(512),
        temperature: None,
        stop_sequences: vec![],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig {
            enabled: true,
            max_uses: None,
            allowed_domains: vec!["docs.rs".into(), "crates.io".into()],
            blocked_domains: vec![],
        },
        tool_choice: None,
    };
    let body = build_test_body(&r, false, None);
    let tools = body["tools"].as_array().expect("tools");
    assert_eq!(tools[0]["allowed_domains"], json!(["docs.rs", "crates.io"]));
    assert!(tools[0].get("blocked_domains").is_none());
}

#[test]
fn web_search_blocked_domains_included() {
    let r = LlmRequest {
        model: "claude-sonnet-4-6".into(),
        system_prompt: None,
        messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})],
        tools: vec![],
        max_tokens: Some(512),
        temperature: None,
        stop_sequences: vec![],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig {
            enabled: true,
            max_uses: None,
            allowed_domains: vec![],
            blocked_domains: vec!["ads.example.com".into()],
        },
        tool_choice: None,
    };
    let body = build_test_body(&r, false, None);
    let tools = body["tools"].as_array().expect("tools");
    assert_eq!(tools[0]["blocked_domains"], json!(["ads.example.com"]));
    assert!(tools[0].get("allowed_domains").is_none());
}

#[test]
fn web_search_allowed_and_blocked_combo_errors() {
    let r = LlmRequest {
        model: "claude-sonnet-4-6".into(),
        system_prompt: None,
        messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})],
        tools: vec![],
        max_tokens: Some(512),
        temperature: None,
        stop_sequences: vec![],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig {
            enabled: true,
            max_uses: None,
            allowed_domains: vec!["example.com".into()],
            blocked_domains: vec!["ads.example.com".into()],
        },
        tool_choice: None,
    };
    let err = build_body(&r, false, None, "claude-sonnet-4-6")
        .expect_err("expected WebSearchDomainConflict");
    assert_eq!(err, RequestBuildError::WebSearchDomainConflict);
}

#[test]
fn provider_block_anthropic_is_echoed_into_messages() {
    let block = json!({
        "type": "web_search_tool_result",
        "tool_use_id": "srvtool_1",
        "content": [{"type": "web_search_result", "url": "https://example.com", "encrypted_content": "enc_abc"}]
    });
    let mut r = req();
    r.messages = vec![
        json!({"role": "user", "content": [{"type": "text", "text": "search for rust"}]}),
        json!({"role": "provider_block", "provider": "anthropic", "block": block}),
    ];
    let body = build_test_body(&r, false, None);
    let msgs = body["messages"].as_array().expect("messages");
    // Should have 2 messages: user and the echoed provider block
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[1]["role"], "user");
    let content = msgs[1]["content"].as_array().expect("content array");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "web_search_tool_result");
    let inner = content[0]["content"]
        .as_array()
        .expect("inner content array");
    assert_eq!(inner[0]["encrypted_content"], "enc_abc");
}

#[test]
fn provider_block_non_anthropic_is_dropped() {
    let block = json!({"type": "something_else", "data": "opaque"});
    let mut r = req();
    r.messages = vec![
        json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
        json!({"role": "provider_block", "provider": "openai", "block": block}),
    ];
    let body = build_test_body(&r, false, None);
    let msgs = body["messages"].as_array().expect("messages");
    // openai provider_block is dropped
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
}

#[test]
fn provider_block_roundtrip_byte_identical() {
    // Multi-turn: a prior turn's anthropic web_search_tool_result block
    // must survive the transform_messages pipeline unchanged byte-for-byte.
    let block = json!({
        "type": "web_search_tool_result",
        "tool_use_id": "srv_42",
        "content": [
            {
                "type": "web_search_result",
                "url": "https://tokio.rs",
                "title": "Tokio",
                "encrypted_content": "encrypted_abc_xyz_123"
            }
        ]
    });
    let original_serialized = serde_json::to_string(&block).expect("serialize block");

    let mut r = req();
    r.messages = vec![
        json!({"role": "user", "content": [{"type": "text", "text": "tell me about tokio"}]}),
        json!({"role": "provider_block", "provider": "anthropic", "block": block}),
    ];
    let body = build_test_body(&r, false, None);
    let msgs = body["messages"].as_array().expect("messages");
    // msgs[1] is the echoed provider block
    let echoed_content = &msgs[1]["content"][0];
    let echoed_serialized = serde_json::to_string(echoed_content).expect("serialize echoed");
    assert_eq!(
        echoed_serialized, original_serialized,
        "provider block content must be byte-identical after roundtrip"
    );
}
