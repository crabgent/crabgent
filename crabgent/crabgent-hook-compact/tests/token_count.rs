use crabgent_core::tokens::IMAGE_TOKENS;
use crabgent_core::{ContentBlock, ImagePayload, Message, Owner, ToolCall};
use crabgent_hook_compact::token_count::estimate_tokens;
use crabgent_test_support::{assistant, user_msg as user};
use serde_json::json;

#[test]
fn estimate_tokens_walks_every_message_variant() {
    let single = estimate_tokens(&[user("hello world")]);
    let many = estimate_tokens(&[
        Message::System {
            content: "system prompt with several words".into(),
        },
        user("user request with several words"),
        assistant("assistant reply with several words"),
        Message::ChannelOutbound {
            conv: Owner::new("slack:T1/C1"),
            body: "outbound message with several words".into(),
            channel: "slack".into(),
            message_id: "1234.5678".into(),
            thread_root: None,
            broadcast: false,
        },
    ]);

    assert!(single > 0, "non-empty text must produce non-zero tokens");
    assert!(
        many > single * 3,
        "four messages should cost more than three copies of one short message"
    );
}

#[test]
fn estimate_tokens_uses_image_constant() {
    let small = Message::User {
        content: vec![ContentBlock::Image(
            ImagePayload::new(vec![1_u8], "image/png").expect("valid image payload"),
        )],
        timestamp: None,
    };
    let large = Message::User {
        content: vec![ContentBlock::Image(
            ImagePayload::new(vec![1_u8; 4096], "image/png").expect("valid image payload"),
        )],
        timestamp: None,
    };

    assert_eq!(estimate_tokens(&[small]), IMAGE_TOKENS);
    assert_eq!(estimate_tokens(&[large]), IMAGE_TOKENS);
}

#[test]
fn estimate_tokens_handles_multibyte_text() {
    let ascii = estimate_tokens(&[user("a".repeat(8))]);
    let cjk = estimate_tokens(&[user("\u{732b}".repeat(8))]);

    assert!(ascii > 0);
    assert!(cjk > 0, "multi-byte text must produce non-zero tokens");
}

#[test]
fn estimate_tokens_walks_tool_result_recursively() {
    let nested_text = Message::ToolResult {
        call_id: "call-1".into(),
        output: json!({"nested": {"output": "abcdefghijklmnop"}}),
        is_error: false,
    };
    let nested_image = Message::ToolResult {
        call_id: "call-2".into(),
        output: json!({
            "image": {
                "type": "image",
                "mime": "image/png",
                "data": "base64-payload-that-is-not-counted-directly"
            }
        }),
        is_error: false,
    };

    assert!(estimate_tokens(&[nested_text]) > 0);
    let image_count = estimate_tokens(&[nested_image]);
    assert!(
        image_count >= IMAGE_TOKENS,
        "image-shaped tool result must score at least IMAGE_TOKENS, got {image_count}"
    );
}

#[test]
fn estimate_tokens_walks_assistant_tool_calls() {
    let with_call = Message::Assistant {
        text: "running tool".into(),
        tool_calls: vec![ToolCall {
            id: "call-abc".into(),
            name: "read_file".into(),
            args: json!({"path": "crabgent-core/src/lib.rs"}),
            thought_signature: None,
        }],
    };

    let bare = assistant("running tool");
    assert!(
        estimate_tokens(&[with_call]) > estimate_tokens(&[bare]),
        "tool-call arguments must add to the assistant cost"
    );
}

#[test]
fn estimate_tokens_walks_scalar_tool_result_variants() {
    let null = Message::ToolResult {
        call_id: "call-null".into(),
        output: json!(null),
        is_error: false,
    };
    let bool_value = Message::ToolResult {
        call_id: "call-bool".into(),
        output: json!(true),
        is_error: false,
    };
    let number = Message::ToolResult {
        call_id: "call-num".into(),
        output: json!(12345),
        is_error: false,
    };
    let array = Message::ToolResult {
        call_id: "call-arr".into(),
        output: json!(["alpha", "beta", "gamma"]),
        is_error: false,
    };
    let plain_string = Message::ToolResult {
        call_id: "call-str".into(),
        output: json!("hello world"),
        is_error: false,
    };
    let empty_object = Message::ToolResult {
        call_id: "call-obj".into(),
        output: json!({}),
        is_error: false,
    };
    let image_missing_data = Message::ToolResult {
        call_id: "call-no-data".into(),
        output: json!({"type": "image", "mime": "image/png"}),
        is_error: false,
    };

    assert_eq!(estimate_tokens(&[null]), 0);
    assert!(estimate_tokens(&[bool_value]) > 0);
    assert!(estimate_tokens(&[number]) > 0);
    assert!(estimate_tokens(&[array]) > 0);
    assert!(estimate_tokens(&[plain_string]) > 0);
    assert_eq!(estimate_tokens(&[empty_object]), 0);
    let missing_data = estimate_tokens(&[image_missing_data]);
    assert!(
        missing_data > 0 && missing_data < IMAGE_TOKENS,
        "image-shaped object without `data` field must not get the image constant, got {missing_data}"
    );
}
