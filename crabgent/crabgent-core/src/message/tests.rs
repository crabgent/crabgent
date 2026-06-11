use super::*;
use serde_json::json;

#[test]
fn system_message_serializes() {
    let m = Message::System {
        content: "be helpful".into(),
    };
    let s = serde_json::to_string(&m).expect("ser");
    assert!(s.contains("\"role\":\"system\""));
    assert!(s.contains("be helpful"));
}

#[test]
fn user_message_with_text_content() {
    let m = Message::user(vec![ContentBlock::Text { text: "hi".into() }]);
    let s = serde_json::to_string(&m).expect("ser");
    assert!(s.contains("\"role\":\"user\""));
    assert!(s.contains("\"type\":\"text\""));
    assert!(
        !s.contains("\"timestamp\""),
        "None timestamp must skip serialization: {s}"
    );
}

#[test]
fn transcript_block_serializes_with_type_tag() {
    let block = ContentBlock::Transcript {
        text: "ja super".into(),
        source_audio: crate::voice::AudioRef::new("01946a3c-7c2b-7d2e-8f1a-5b3c2d1e0f0a"),
        voice: None,
    };
    let s = serde_json::to_string(&block).expect("ser");
    assert!(s.contains("\"type\":\"transcript\""), "{s}");
    assert!(
        s.contains("\"source_audio\":\"01946a3c-7c2b-7d2e-8f1a-5b3c2d1e0f0a\""),
        "{s}"
    );
    assert!(s.contains("\"voice\":null"), "{s}");
}

#[test]
fn transcript_block_roundtrips_without_voice() {
    let block = ContentBlock::Transcript {
        text: "hi".into(),
        source_audio: crate::voice::AudioRef::new("ref-1"),
        voice: None,
    };
    let back: ContentBlock =
        serde_json::from_str(&serde_json::to_string(&block).expect("ser")).expect("de");
    assert_eq!(block, back);
}

#[test]
fn transcript_block_roundtrips_with_voice() {
    let block = ContentBlock::Transcript {
        text: "mhm".into(),
        source_audio: crate::voice::AudioRef::new("ref-2"),
        voice: Some(crate::voice::VoiceSignals {
            pause_ms: Some(900),
            hesitation_count: 1,
            ..Default::default()
        }),
    };
    let back: ContentBlock =
        serde_json::from_str(&serde_json::to_string(&block).expect("ser")).expect("de");
    assert_eq!(block, back);
}

#[test]
fn transcript_block_deserializes_with_missing_voice() {
    let json = r#"{"type":"transcript","text":"hi","source_audio":"ref-3"}"#;
    let block: ContentBlock = serde_json::from_str(json).expect("de");
    match block {
        ContentBlock::Transcript {
            voice,
            source_audio,
            text,
        } => {
            assert!(voice.is_none());
            assert_eq!(source_audio.as_str(), "ref-3");
            assert_eq!(text, "hi");
        }
        other => panic!("expected Transcript, got {other:?}"),
    }
}

#[test]
fn user_message_with_timestamp_serializes_iso() {
    let when = DateTime::parse_from_rfc3339("2026-05-21T14:32:11Z")
        .expect("rfc3339")
        .with_timezone(&Utc);
    let m = Message::user_at(vec![ContentBlock::Text { text: "hi".into() }], when);
    let s = serde_json::to_string(&m).expect("ser");
    assert!(s.contains("\"timestamp\":\"2026-05-21T14:32:11Z\""), "{s}");
}

#[test]
fn user_message_backcompat_missing_timestamp_deserializes_none() {
    let json = r#"{"role":"user","content":[{"type":"text","text":"hi"}]}"#;
    let m: Message = serde_json::from_str(json).expect("de");
    match m {
        Message::User { timestamp, .. } => assert!(timestamp.is_none()),
        _ => panic!("expected User"),
    }
}

#[test]
fn user_message_timestamp_roundtrips() {
    let when = DateTime::parse_from_rfc3339("2026-05-21T14:32:11+02:00")
        .expect("rfc3339")
        .with_timezone(&Utc);
    let m = Message::user_at(vec![ContentBlock::Text { text: "hi".into() }], when);
    let s = serde_json::to_string(&m).expect("ser");
    let back: Message = serde_json::from_str(&s).expect("de");
    match back {
        Message::User {
            timestamp: Some(t), ..
        } => assert_eq!(t, when),
        other => panic!("expected User with timestamp, got {other:?}"),
    }
}

#[test]
fn assistant_message_with_tool_call() {
    let m = Message::Assistant {
        text: "checking".into(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            args: json!({}),
            thought_signature: None,
        }],
    };
    let s = serde_json::to_string(&m).expect("ser");
    assert!(s.contains("\"role\":\"assistant\""));
    assert!(s.contains("\"id\":\"c1\""));
}

#[test]
fn tool_result_message() {
    let m = Message::ToolResult {
        call_id: "c1".into(),
        output: json!({"ok": true}),
        is_error: false,
    };
    let s = serde_json::to_string(&m).expect("ser");
    assert!(s.contains("\"role\":\"tool_result\""));
    assert!(s.contains("\"call_id\":\"c1\""));
}

#[test]
fn content_block_image_serializes_with_type_tag() {
    let cb = ContentBlock::Image(
        ImagePayload::new(b"png".to_vec(), "image/png").expect("valid image payload"),
    );
    let s = serde_json::to_string(&cb).expect("ser");
    assert!(s.contains("\"type\":\"image\""));
    assert!(s.contains("\"mime\":\"image/png\""));
    assert!(s.contains("\"data\":\"cG5n\""));
}

#[test]
fn content_block_image_json_shape_neutral() {
    let block = ContentBlock::Image(
        ImagePayload::new(b"gif".to_vec(), "image/gif").expect("valid image payload"),
    );

    let value = serde_json::to_value(&block).expect("ser");

    assert_eq!(
        value,
        json!({"type": "image", "mime": "image/gif", "data": "Z2lm"})
    );
    assert!(value.get("source").is_none());
}

#[test]
fn content_block_audio_json_shape_neutral() {
    let block = ContentBlock::Audio(
        AudioPayload::new(b"wav".to_vec(), "audio/wav", Some("clip.wav".into()))
            .expect("valid audio payload"),
    );

    let value = serde_json::to_value(&block).expect("ser");

    assert_eq!(
        value,
        json!({
            "type": "audio",
            "mime": "audio/wav",
            "data": "d2F2",
            "filename": "clip.wav"
        })
    );
}

#[test]
fn content_block_file_json_shape_neutral() {
    let block = ContentBlock::File(
        FilePayload::new(b"txt".to_vec(), "text/plain", "note.txt").expect("valid file payload"),
    );

    let value = serde_json::to_value(&block).expect("ser");

    assert_eq!(
        value,
        json!({
            "type": "file",
            "mime": "text/plain",
            "data": "dHh0",
            "filename": "note.txt"
        })
    );
}

#[test]
fn content_block_image_partial_eq_byte_wise() {
    let left = ContentBlock::Image(
        ImagePayload::new(vec![137, 80, 78, 71], "image/png").expect("valid image payload"),
    );
    let right = ContentBlock::Image(
        ImagePayload::new(vec![137, 80, 78, 71], "image/png").expect("valid image payload"),
    );
    let different = ContentBlock::Image(
        ImagePayload::new(vec![71, 73, 70], "image/gif").expect("valid image payload"),
    );

    assert_eq!(left, right);
    assert_ne!(left, different);
}

#[test]
fn round_trip_typed_to_raw() {
    let typed = vec![
        Message::System {
            content: "you are X".into(),
        },
        Message::user(vec![ContentBlock::Text { text: "hi".into() }]),
    ];
    let raw: RawMessages = typed.into();
    let back: Vec<Message> = raw.try_into().expect("try_from");
    assert_eq!(back.len(), 2);
    assert!(matches!(back[0], Message::System { .. }));
    assert!(matches!(back[1], Message::User { .. }));
}

#[test]
fn raw_messages_empty_default() {
    let r = RawMessages::default();
    assert_eq!(r.len(), 0);
    assert!(r.is_empty());
    assert_eq!(r.as_slice().len(), 0);
}

#[test]
fn raw_messages_push_grows() {
    let mut r = RawMessages::new();
    r.push(json!({"role": "user", "content": [{"type": "text", "text": "x"}]}));
    assert_eq!(r.len(), 1);
    assert!(!r.is_empty());
}

#[test]
fn raw_messages_into_inner_returns_vec() {
    let r = RawMessages(vec![json!(1), json!(2)]);
    let v = r.into_inner();
    assert_eq!(v.len(), 2);
}

#[test]
fn try_from_raw_with_invalid_shape_fails() {
    let r = RawMessages(vec![json!({"unknown": "shape"})]);
    let result: Result<Vec<Message>, _> = r.try_into();
    result.expect_err("expected error");
}

#[test]
fn channel_outbound_serde_roundtrip() {
    let m = Message::ChannelOutbound {
        conv: Owner::new("slack:T1/C1"),
        body: "hello world".into(),
        channel: "slack".into(),
        message_id: "1234.5678".into(),
        thread_root: None,
        broadcast: false,
    };
    let s = serde_json::to_string(&m).expect("ser");
    assert!(s.contains("\"role\":\"channel_outbound\""));
    assert!(s.contains("\"body\":\"hello world\""));
    let back: Message = serde_json::from_str(&s).expect("de");
    assert!(matches!(back, Message::ChannelOutbound { .. }));
    if let Message::ChannelOutbound {
        conv,
        body,
        channel,
        message_id,
        thread_root,
        broadcast,
    } = back
    {
        assert_eq!(conv.as_str(), "slack:T1/C1");
        assert_eq!(body, "hello world");
        assert_eq!(channel, "slack");
        assert_eq!(message_id, "1234.5678");
        assert!(thread_root.is_none());
        assert!(!broadcast);
    }
}

#[test]
fn from_typed_uses_role_tag() {
    let typed = vec![Message::System {
        content: "x".into(),
    }];
    let raw: RawMessages = typed.into();
    let first = &raw.as_slice()[0];
    assert_eq!(first["role"], "system");
    assert_eq!(first["content"], "x");
}
