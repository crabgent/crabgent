use crabgent_core::LlmRequest;
use crabgent_provider_anthropic::request::build_body;
use serde_json::json;

fn req(messages: Vec<serde_json::Value>) -> LlmRequest {
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

fn build_test_body(req: &LlmRequest, stream: bool, cache_ttl: Option<&str>) -> serde_json::Value {
    build_body(req, stream, cache_ttl, req.model.as_str()).expect("build_body")
}

#[test]
fn transform_user_image_block_wraps_into_source() {
    let req = req(vec![json!({
        "role": "user",
        "content": [
            {"type": "image", "mime": "image/png", "data": "iVBORw0K"}
        ],
    })]);

    let body = build_test_body(&req, true, None);
    assert_eq!(
        body["messages"][0]["content"],
        json!([{
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": "iVBORw0K",
            },
        }])
    );
}

#[test]
fn transform_user_message_text_only_unchanged() {
    let req = req(vec![json!({
        "role": "user",
        "content": [{"type": "text", "text": "hello vision"}],
    })]);

    let body = build_test_body(&req, true, None);
    assert_eq!(
        body["messages"][0]["content"],
        json!([{"type": "text", "text": "hello vision"}])
    );
}

#[test]
fn transform_user_message_multi_image_blocks() {
    let req = req(vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "compare"},
            {"type": "image", "mime": "image/png", "data": "aGVsbG8="},
            {"type": "image", "mime": "image/jpeg", "data": "a2hzb3B"},
        ],
    })]);

    let body = build_test_body(&req, true, None);
    assert_eq!(
        body["messages"][0]["content"],
        json!([
            {"type": "text", "text": "compare"},
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": "aGVsbG8=",
                },
            },
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/jpeg",
                    "data": "a2hzb3B",
                },
            },
        ])
    );
}

#[test]
fn anthropic_request_skips_image_block_missing_mime() {
    let req = req(vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "keep"},
            {"type": "image", "data": "aGVsbG8="},
        ],
    })]);

    let body = build_test_body(&req, true, None);
    assert_eq!(
        body["messages"][0]["content"],
        json!([{"type": "text", "text": "keep"}])
    );
}

#[test]
fn anthropic_request_skips_image_block_missing_data() {
    let req = req(vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "keep"},
            {"type": "image", "mime": "image/png"},
        ],
    })]);

    let body = build_test_body(&req, true, None);
    assert_eq!(
        body["messages"][0]["content"],
        json!([{"type": "text", "text": "keep"}])
    );
}

#[test]
fn anthropic_request_drops_user_message_with_only_malformed_images() {
    let req = req(vec![json!({
        "role": "user",
        "content": [
            {"type": "image", "data": "aGVsbG8="},
            {"type": "image", "mime": "image/png"},
        ],
    })]);

    let body = build_test_body(&req, true, None);
    assert_eq!(body["messages"], json!([]));
}
