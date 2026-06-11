use super::*;
use httpmock::Method::POST;
use httpmock::MockServer;
use serde_json::json;

fn build_channel(server: &MockServer) -> TelegramChannel {
    TelegramChannel::new("test-token", "B-1", "crabgent_bot").with_api_base(server.base_url())
}

fn thread_parent(conv: &str, id: &str, root: &str) -> MessageRef {
    MessageRef::thread_reply_broadcast("telegram", Owner::new(conv), id, root, false)
}

#[tokio::test]
async fn kind_is_always_direct() {
    let server = MockServer::start();
    let c = build_channel(&server);
    let k = c
        .kind(&Owner::new("telegram:42"))
        .await
        .expect("test result");
    assert_eq!(k, ChannelKind::Direct);
}

#[tokio::test]
async fn direct_role_is_human_agent() {
    let server = MockServer::start();
    let c = build_channel(&server);
    let r = c
        .direct_role(&Owner::new("telegram:42"))
        .await
        .expect("test result");
    assert_eq!(r, Some(DirectRole::HumanAgent));
}

#[tokio::test]
async fn participants_returns_bot_plus_user() {
    let server = MockServer::start();
    let c = build_channel(&server);
    let s = Subject::new("agent");
    let parts = c
        .participants(&s, &Owner::new("telegram:42"))
        .await
        .expect("test result");
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].role, ParticipantRole::Bot);
    assert_eq!(parts[0].id.as_str(), "B-1");
    assert_eq!(parts[0].display_name.as_deref(), Some("crabgent_bot"));
    assert_eq!(parts[1].role, ParticipantRole::Human);
    assert_eq!(parts[1].id.as_str(), "user:42");
}

#[tokio::test]
async fn send_top_level_dispatches_correct_payload() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/sendMessage")
            .json_body(json!({
                "chat_id": 42,
                "text": "Use <code>hi</code>",
                "parse_mode": "HTML"
            }));
        then.status(200).json_body(json!({
            "ok": true,
            "result": {
                "message_id": 1700,
                "chat": { "id": 42 }
            }
        }));
    });
    let c = build_channel(&server);
    let s = Subject::new("agent");
    let m = OutboundMessage::new("Use <code>hi</code>");
    let r = c
        .send(&s, &Owner::new("telegram:42"), &m)
        .await
        .expect("test result");
    mock.assert();
    assert_eq!(r.id, "1700");
    assert_eq!(r.thread_root, None);
    assert_eq!(r.conv.as_str(), "telegram:42");
}

#[tokio::test]
async fn send_markdown_dispatches_html_payload() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/sendMessage")
            .json_body(json!({
                "chat_id": 42,
                "text": "<b>Wetter</b>\n<b>Regen</b> bei <a href=\"https://dwd.de\">DWD</a>.",
                "parse_mode": "HTML"
            }));
        then.status(200).json_body(json!({
            "ok": true,
            "result": {
                "message_id": 1700,
                "chat": { "id": 42 }
            }
        }));
    });
    let c = build_channel(&server);
    let s = Subject::new("agent");
    let m = OutboundMessage::new("## Wetter\n**Regen** bei [DWD](https://dwd.de).");
    let r = c
        .send(&s, &Owner::new("telegram:42"), &m)
        .await
        .expect("test result");
    mock.assert();
    assert_eq!(r.id, "1700");
}

#[tokio::test]
async fn send_with_thread_parent_includes_thread_id() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/sendMessage")
            .json_body(json!({
                "chat_id": 42,
                "text": "reply",
                "message_thread_id": 99
            }));
        then.status(200).json_body(json!({
            "ok": true,
            "result": {
                "message_id": 1701,
                "chat": { "id": 42 },
                "message_thread_id": 99
            }
        }));
    });
    let c = build_channel(&server);
    let s = Subject::new("agent");
    let parent = thread_parent("telegram:42", "1700", "99");
    let m = OutboundMessage::new("reply").in_thread(parent);
    let r = c
        .send(&s, &Owner::new("telegram:42"), &m)
        .await
        .expect("test result");
    mock.assert();
    assert_eq!(r.thread_root.as_deref(), Some("99"));
}

#[tokio::test]
async fn send_caps_body_at_char_count() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/sendMessage")
            .json_body(json!({
                "chat_id": 42,
                "text": "äbcäbcäbcä"
            }));
        then.status(200).json_body(json!({
            "ok": true,
            "result": {
                "message_id": 1702,
                "chat": { "id": 42 }
            }
        }));
    });
    let c = build_channel(&server).with_body_cap_chars(10);
    let s = Subject::new("agent");
    let m = OutboundMessage::new("äbc".repeat(20));

    let r = c
        .send(&s, &Owner::new("telegram:42"), &m)
        .await
        .expect("test result");

    mock.assert();
    assert_eq!(r.id, "1702");
}

#[tokio::test]
async fn edit_dispatches_correct_payload() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/editMessageText")
            .json_body(json!({
                "chat_id": 42,
                "message_id": 1700,
                "text": "updated <code>x</code>",
                "parse_mode": "HTML"
            }));
        then.status(200).json_body(json!({
            "ok": true,
            "result": {
                "message_id": 1700,
                "chat": { "id": 42 },
                "text": "updated"
            }
        }));
    });
    let c = build_channel(&server);
    let target = MessageRef::top_level("telegram", Owner::new("telegram:42"), "1700");

    c.edit(
        &Subject::new("agent"),
        &Owner::new("telegram:42"),
        &target,
        "updated <code>x</code>",
    )
    .await
    .expect("edit");

    mock.assert();
}

#[tokio::test]
async fn edit_markdown_dispatches_html_payload() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/editMessageText")
            .json_body(json!({
                "chat_id": 42,
                "message_id": 1700,
                "text": "<b>updated</b> <code>x</code>",
                "parse_mode": "HTML"
            }));
        then.status(200).json_body(json!({
            "ok": true,
            "result": {
                "message_id": 1700,
                "chat": { "id": 42 },
                "text": "updated"
            }
        }));
    });
    let c = build_channel(&server);
    let target = MessageRef::top_level("telegram", Owner::new("telegram:42"), "1700");

    c.edit(
        &Subject::new("agent"),
        &Owner::new("telegram:42"),
        &target,
        "**updated** `x`",
    )
    .await
    .expect("edit");

    mock.assert();
}

#[tokio::test]
async fn delete_dispatches_correct_payload() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/deleteMessage")
            .json_body(json!({
                "chat_id": 42,
                "message_id": 1700
            }));
        then.status(200)
            .json_body(json!({"ok": true, "result": true}));
    });
    let c = build_channel(&server);
    let target = MessageRef::top_level("telegram", Owner::new("telegram:42"), "1700");

    c.delete(&Subject::new("agent"), &Owner::new("telegram:42"), &target)
        .await
        .expect("delete");

    mock.assert();
}

#[tokio::test]
async fn upload_dispatches_multipart_document_payload() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/bottest-token/sendDocument")
            .body_matches(r#"name="chat_id""#)
            .body_matches("42")
            .body_matches(r#"name="document"; filename="note.txt""#)
            .body_matches("hello")
            .body_matches(r#"name="caption""#)
            .body_matches("caption <code>x</code>")
            .body_matches(r#"name="parse_mode""#)
            .body_matches("HTML")
            .body_matches(r#"name="reply_to_message_id""#)
            .body_matches("1700");
        then.status(200).json_body(json!({
            "ok": true,
            "result": {
                "message_id": 1701,
                "chat": { "id": 42 }
            }
        }));
    });
    let c = build_channel(&server);
    let conv = Owner::new("telegram:42");
    let parent = MessageRef::top_level("telegram", conv.clone(), "1700");

    let uploaded = c
        .upload(
            &Subject::new("agent"),
            &conv,
            "note.txt",
            b"hello".to_vec(),
            Some("caption <code>x</code>"),
            Some(&parent),
        )
        .await
        .expect("upload");

    mock.assert();
    assert_eq!(uploaded.id, "1701");
    assert_eq!(uploaded.thread_root(), Some("1700"));
}

#[tokio::test]
async fn read_keeps_default_unsupported() {
    let server = MockServer::start();
    let c = build_channel(&server);
    let err = c
        .read(&Subject::new("agent"), &Owner::new("telegram:42"), None, 10)
        .await
        .expect_err("telegram history read is unsupported");

    assert!(matches!(err, ChannelError::Unsupported("read")));
}

#[tokio::test]
async fn send_with_non_ok_response_returns_error() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottest-token/sendMessage");
        then.status(200)
            .json_body(json!({"ok": false, "description": "chat not found"}));
    });
    let c = build_channel(&server);
    let s = Subject::new("agent");
    let m = OutboundMessage::new("hi");
    let err = c
        .send(&s, &Owner::new("telegram:42"), &m)
        .await
        .expect_err("must fail");
    assert!(matches!(err, ChannelError::Adapter(_)));
}

#[tokio::test]
async fn send_with_5xx_returns_adapter_error() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/bottest-token/sendMessage");
        then.status(503);
    });
    let c = build_channel(&server);
    let s = Subject::new("agent");
    let m = OutboundMessage::new("hi");
    let err = c
        .send(&s, &Owner::new("telegram:42"), &m)
        .await
        .expect_err("fail");
    assert!(matches!(err, ChannelError::Adapter(_)));
}

#[tokio::test]
async fn reqwest_error_does_not_leak_bot_token() {
    let token = "123456:SECRET_TOKEN";
    let c = TelegramChannel::new(token, "B-1", "crabgent_bot").with_api_base("http://127.0.0.1:1");

    let err = c
        .post_json("sendMessage", &json!({"chat_id": 42, "text": "hi"}))
        .await
        .expect_err("closed local port must fail");
    let rendered = err.to_string();

    assert!(!rendered.contains(token), "{rendered}");
    assert!(!rendered.contains(&format!("bot{token}")), "{rendered}");
}

#[test]
fn adapter_error_redacts_bot_token_from_message() {
    let token = "123456:SECRET_TOKEN";
    let c = TelegramChannel::new(token, "B-1", "crabgent_bot");
    let err = c.adapter_error(format!("telegram failed at /bot{token}/getUpdates"));

    // `ChannelError::Adapter` Display is opaque for LLM-safety; the
    // underlying (and already token-redacted) message lives in the
    // inner field for Debug + tracing. Verify the redaction by
    // destructuring rather than via Display.
    let ChannelError::Adapter(detail) = &err else {
        panic!("expected Adapter, got {err:?}");
    };
    assert!(!detail.contains(token), "{detail}");
    assert!(detail.contains("/bot<redacted>/getUpdates"), "{detail}");
}

#[tokio::test]
async fn send_with_invalid_thread_root_returns_envelope_error() {
    let server = MockServer::start();
    let c = build_channel(&server);
    let s = Subject::new("agent");
    let parent = thread_parent("telegram:42", "1700", "abc");
    let m = OutboundMessage::new("reply").in_thread(parent);
    let err = c
        .send(&s, &Owner::new("telegram:42"), &m)
        .await
        .expect_err("fail");
    assert!(matches!(err, ChannelError::InvalidEnvelope(_)));
}

#[tokio::test]
async fn send_with_unparseable_conv_returns_not_found() {
    let server = MockServer::start();
    let c = build_channel(&server);
    let s = Subject::new("agent");
    let m = OutboundMessage::new("hi");
    let err = c
        .send(&s, &Owner::new("telegram:abc"), &m)
        .await
        .expect_err("fail");
    assert!(matches!(err, ChannelError::ConversationNotFound(_)));
}

#[test]
fn channel_name_constant_matches_trait_method() {
    let server = MockServer::start();
    let c = build_channel(&server);
    assert_eq!(c.name(), CHANNEL_NAME);
    assert_eq!(CHANNEL_NAME, "telegram");
}

#[tokio::test]
async fn constructor_stores_identity_for_participants() {
    let server = MockServer::start();
    let c = TelegramChannel::new("tk", "B-2", "tester")
        .with_api_base(server.base_url())
        .with_client(reqwest::Client::new());
    let s = Subject::new("agent");
    let parts = c
        .participants(&s, &Owner::new("telegram:1"))
        .await
        .expect("test result");
    assert_eq!(parts[0].id.as_str(), "B-2");
    assert_eq!(parts[0].display_name.as_deref(), Some("tester"));
}

#[test]
fn api_base_is_readable() {
    let server = MockServer::start();
    let c = build_channel(&server);
    assert!(c.api_base().starts_with("http"));
}
