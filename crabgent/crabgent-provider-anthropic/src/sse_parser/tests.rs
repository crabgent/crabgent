//! Tests for `sse_parser`.
use super::*;
use crabgent_core::StopReason;

fn feed_string(parser: &mut SseParser, s: &str) -> Vec<ParserResult> {
    parser.feed(s.as_bytes())
}

#[test]
fn parses_text_delta_emits_token() {
    let mut p = SseParser::new();
    feed_string(
        &mut p,
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
    );
    let evs = feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
    );
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        Ok(ProviderEvent::TextDelta(s)) => assert_eq!(s, "Hello"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_tool_use_with_streamed_input() {
    let mut p = SseParser::new();
    feed_string(
        &mut p,
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"echo\"}}\n\n",
    );
    feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"k\\\":\"}}\n\n",
    );
    feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"v\\\"}\"}}\n\n",
    );
    let evs = feed_string(
        &mut p,
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    );
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        Ok(ProviderEvent::ToolUse(call)) => {
            assert_eq!(call.id, "toolu_1");
            assert_eq!(call.name, "echo");
            assert_eq!(call.args, json!({"k": "v"}));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_thinking_delta_emits_reasoning() {
    let mut p = SseParser::new();
    feed_string(
        &mut p,
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
    );
    let evs = feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"chain-of-thought\"}}\n\n",
    );
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        Ok(ProviderEvent::ReasoningDelta(s)) => assert_eq!(s, "chain-of-thought"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_mixed_thinking_text_tool() {
    let mut p = SseParser::new();
    // thinking block at index 0
    feed_string(
        &mut p,
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
    );
    let evs_t = feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"reason\"}}\n\n",
    );
    assert!(matches!(
        evs_t.as_slice(),
        [Ok(ProviderEvent::ReasoningDelta(s))] if s == "reason"
    ));
    feed_string(
        &mut p,
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    );

    // text block at index 1
    feed_string(
        &mut p,
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\"}}\n\n",
    );
    let evs_text = feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"answer\"}}\n\n",
    );
    assert!(matches!(
        evs_text.as_slice(),
        [Ok(ProviderEvent::TextDelta(s))] if s == "answer"
    ));
    feed_string(
        &mut p,
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
    );

    // tool_use block at index 2
    feed_string(
        &mut p,
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_x\",\"name\":\"echo\"}}\n\n",
    );
    feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"k\\\":\\\"v\\\"}\"}}\n\n",
    );
    let evs_tool = feed_string(
        &mut p,
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
    );
    assert!(matches!(
        evs_tool.as_slice(),
        [Ok(ProviderEvent::ToolUse(call))] if call.id == "toolu_x" && call.name == "echo"
    ));
}

#[test]
fn end_of_thinking_block_emits_nothing() {
    let mut p = SseParser::new();
    feed_string(
        &mut p,
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
    );
    feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"a\"}}\n\n",
    );
    let evs = feed_string(
        &mut p,
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    );
    assert!(evs.is_empty());
}

#[test]
fn thinking_delta_without_block_start_returns_none() {
    let mut p = SseParser::new();
    let evs = feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":7,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"orphan\"}}\n\n",
    );
    assert!(evs.is_empty());
}

#[test]
fn message_start_records_cache_input_tokens() {
    let mut p = SseParser::new();
    feed_string(
        &mut p,
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"claude\",\"usage\":{\"input_tokens\":12,\"cache_creation_input_tokens\":34,\"cache_read_input_tokens\":56,\"output_tokens\":0}}}\n\n",
    );
    let evs = feed_string(
        &mut p,
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
    );
    assert!(!evs.is_empty(), "expected Usage event after stop");
    let Ok(ProviderEvent::Usage(usage)) = &evs[0] else {
        panic!("expected Usage event, got {:?}", evs[0]);
    };
    assert_eq!(usage.input_tokens, 12);
    assert_eq!(usage.cache_creation_tokens, 34);
    assert_eq!(usage.cache_read_tokens, 56);
    assert_eq!(usage.output_tokens, 7);
}

#[test]
fn message_delta_emits_stop_reason_and_usage() {
    let mut p = SseParser::new();
    let evs = feed_string(
        &mut p,
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}\n\n",
    );
    assert_eq!(evs.len(), 2);
    assert!(matches!(evs[0], Ok(ProviderEvent::Usage(_))));
    match &evs[1] {
        Ok(ProviderEvent::Stop(StopReason::EndTurn)) => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn malformed_tool_input_yields_error() {
    let mut p = SseParser::new();
    feed_string(
        &mut p,
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"id1\",\"name\":\"x\"}}\n\n",
    );
    feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{not-json\"}}\n\n",
    );
    let evs = feed_string(
        &mut p,
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    );
    assert_eq!(evs.len(), 1);
    assert!(matches!(&evs[0], Err(err) if err.message().contains("malformed input")));
}

#[test]
fn overloaded_error_is_retryable() {
    let mut p = SseParser::new();
    let evs = feed_string(
        &mut p,
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}\n\n",
    );

    assert_eq!(evs.len(), 1);
    assert!(matches!(&evs[0], Err(err) if err.is_retryable() && err.message().contains("busy")));
}

#[test]
fn stream_error_message_redacts_configured_api_key() {
    // Custom-format key (not sk-/secret-prefixed) to prove the exact-match
    // redaction path, independent of the pattern-based span scrubber.
    let api_key = "ant-custom-key-abc123";
    let mut p = SseParser::new().with_api_key(api_key);
    let evs = feed_string(
        &mut p,
        &format!(
            "event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"invalid_request_error\",\"message\":\"bad key {api_key} rejected\"}}}}\n\n"
        ),
    );

    assert_eq!(evs.len(), 1);
    let Err(err) = &evs[0] else {
        panic!("expected an SseError from a stream error event");
    };
    let surfaced = err.message();
    assert!(
        !surfaced.contains(api_key),
        "api key leaked into surfaced error: {surfaced}"
    );
    assert!(
        surfaced.contains("[REDACTED]"),
        "redaction marker missing: {surfaced}"
    );
    // The error type is not secret and stays for caller context.
    assert!(surfaced.contains("invalid_request_error"));
}

#[test]
fn stream_error_message_redacts_secret_pattern_without_key() {
    // No configured key: the pattern scrubber must still strip an
    // sk-ant-prefixed token echoed in a server-controlled error message.
    let mut p = SseParser::new();
    let evs = feed_string(
        &mut p,
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"token sk-ant-api03-LEAKEDSECRET invalid\"}}\n\n",
    );

    assert_eq!(evs.len(), 1);
    let Err(err) = &evs[0] else {
        panic!("expected an SseError from a stream error event");
    };
    let surfaced = err.message();
    assert!(
        !surfaced.contains("sk-ant-api03-LEAKEDSECRET"),
        "secret-like token leaked: {surfaced}"
    );
    assert!(surfaced.contains("[REDACTED]"));
    assert!(surfaced.contains("authentication_error"));
}

#[test]
fn unknown_event_type_is_ignored() {
    let mut p = SseParser::new();
    let evs = feed_string(&mut p, "event: ping\ndata: {\"x\":1}\n\n");
    assert!(evs.is_empty());
}

#[test]
fn non_json_data_is_dropped() {
    let mut p = SseParser::new();
    let evs = feed_string(&mut p, "event: content_block_delta\ndata: not-json\n\n");
    assert!(evs.is_empty());
}

#[test]
fn multi_byte_utf8_split_across_chunks_decodes() {
    let mut p = SseParser::new();
    // "ä" = 0xc3 0xa4. Split between bytes; parser must hold the
    // first byte until the second arrives.
    let line = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n";
    p.feed(line.as_bytes());
    let mid = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"";
    p.feed(mid.as_bytes());
    let bytes = "ä".as_bytes();
    p.feed(&bytes[..1]);
    let evs = p.feed(&[bytes[1], b'"', b'}', b'}', b'\n', b'\n']);
    assert!(
        evs.iter()
            .any(|e| matches!(e, Ok(ProviderEvent::TextDelta(s)) if s.contains('ä')))
    );
}

#[test]
fn stop_reason_mapping_covers_known_values() {
    assert!(matches!(map_stop_reason("end_turn"), StopReason::EndTurn));
    assert!(matches!(map_stop_reason(""), StopReason::EndTurn));
    assert!(matches!(map_stop_reason("tool_use"), StopReason::ToolUse));
    assert!(matches!(
        map_stop_reason("max_tokens"),
        StopReason::MaxTokens
    ));
    assert!(matches!(
        map_stop_reason("stop_sequence"),
        StopReason::StopSequence
    ));
    assert!(matches!(
        map_stop_reason("error:overloaded_error"),
        StopReason::Other
    ));
}

#[test]
fn finish_emits_terminal_events_when_stream_aborts_early() {
    let p = SseParser::new();
    let evs = p.finish();
    assert_eq!(evs.len(), 2);
    assert!(matches!(evs[0], Ok(ProviderEvent::Usage(_))));
    assert!(matches!(
        evs[1],
        Ok(ProviderEvent::Stop(StopReason::EndTurn))
    ));
}

#[test]
fn line_buffer_overflow_resets_state() {
    let mut p = SseParser::new();
    // Push >2MB of garbage without a newline.
    let big = "x".repeat(2 * 1024 * 1024 + 1);
    let evs = p.feed(big.as_bytes());
    assert!(matches!(&evs[0], Err(err) if err.message().contains("overflow")));
}

#[test]
fn index_parse_handles_missing_or_invalid() {
    assert_eq!(parse_index(&json!({"index": 5})), Some(5));
    assert_eq!(parse_index(&json!({"index": "x"})), None);
    assert_eq!(parse_index(&json!({})), None);
}

#[test]
fn append_respects_limits() {
    let limits = ParserLimits {
        block_content_bytes: 4,
        total_content_bytes: 8,
    };
    let mut buf = String::new();
    let mut total = 0usize;
    assert!(append_within_limits(&mut buf, "abcd", &limits, &mut total));
    assert!(!append_within_limits(&mut buf, "x", &limits, &mut total));
}

#[test]
fn max_block_builders_emits_error_event() {
    let mut p = SseParser::new();
    for index in 0..MAX_BLOCK_BUILDERS {
        let evs = feed_string(
            &mut p,
            &format!(
                "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":{index},\"content_block\":{{\"type\":\"text\"}}}}\n\n"
            ),
        );
        assert!(evs.is_empty());
    }

    let evs = feed_string(
        &mut p,
        &format!(
            "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":{MAX_BLOCK_BUILDERS},\"content_block\":{{\"type\":\"text\"}}}}\n\n"
        ),
    );

    assert_eq!(evs.len(), 1);
    assert!(matches!(
        &evs[0],
        Err(err) if err.message().contains("max content block builders reached")
    ));
}
