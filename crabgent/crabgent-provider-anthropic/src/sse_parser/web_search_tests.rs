//! Web search SSE tests.

use super::*;

fn feed_string(parser: &mut SseParser, s: &str) -> Vec<ParserResult> {
    parser.feed(s.as_bytes())
}

#[test]
fn server_tool_use_block_stop_emits_no_event() {
    let mut p = SseParser::new();
    feed_string(
        &mut p,
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"server_tool_use\",\"id\":\"srvtool_1\",\"name\":\"web_search_20250305\"}}\n\n",
    );
    feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\":\"}}\n\n",
    );
    feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"rust lang\\\"}\" }}\n\n",
    );
    let evs = feed_string(
        &mut p,
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
    );
    // server_tool_use stop does NOT emit an event; the result block follows.
    assert!(
        evs.is_empty(),
        "expected no event on server_tool_use stop, got {evs:?}"
    );
}

#[test]
fn web_search_tool_result_emits_server_tool_result() {
    let mut p = SseParser::new();
    // The full result block arrives in content_block_start (Anthropic sends it inline).
    let result_block = serde_json::json!({
        "type": "web_search_tool_result",
        "tool_use_id": "srvtool_1",
        "content": [
            {
                "type": "web_search_result",
                "url": "https://example.com",
                "title": "Example",
                "encrypted_content": "enc_abc123"
            }
        ]
    });
    let start_payload = serde_json::json!({
        "type": "content_block_start",
        "index": 2,
        "content_block": result_block
    });
    feed_string(
        &mut p,
        &format!("event: content_block_start\ndata: {start_payload}\n\n"),
    );
    let evs = feed_string(
        &mut p,
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
    );
    assert_eq!(evs.len(), 1, "expected ServerToolResult event, got {evs:?}");
    match &evs[0] {
        Ok(ProviderEvent::ServerToolResult {
            provider,
            name,
            content,
            citations,
        }) => {
            assert_eq!(provider, "anthropic");
            assert_eq!(name, "web_search");
            // encrypted_content preserved verbatim
            let enc = content["content"][0]["encrypted_content"].as_str();
            assert_eq!(enc, Some("enc_abc123"), "encrypted_content not preserved");
            assert!(citations.is_empty(), "no citations expected here");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn web_search_tool_result_extracts_citations() {
    let mut p = SseParser::new();
    let result_block = serde_json::json!({
        "type": "web_search_tool_result",
        "tool_use_id": "srvtool_2",
        "content": [
            {
                "type": "text",
                "text": "some cited text",
                "citations": [
                    {
                        "type": "web_search_result_location",
                        "url": "https://docs.rs/tokio",
                        "title": "tokio docs",
                        "cited_text": "async runtime"
                    }
                ]
            }
        ]
    });
    let start_payload = serde_json::json!({
        "type": "content_block_start",
        "index": 3,
        "content_block": result_block
    });
    feed_string(
        &mut p,
        &format!("event: content_block_start\ndata: {start_payload}\n\n"),
    );
    let evs = feed_string(
        &mut p,
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":3}\n\n",
    );
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        Ok(ProviderEvent::ServerToolResult { citations, .. }) => {
            assert_eq!(citations.len(), 1, "expected 1 citation");
            let c = &citations[0];
            assert_eq!(c.url, "https://docs.rs/tokio");
            assert_eq!(c.title.as_deref(), Some("tokio docs"));
            assert_eq!(c.cited_text.as_deref(), Some("async runtime"));
            assert_eq!(c.provider, "anthropic");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn full_web_search_sse_roundtrip() {
    // text block + server_tool_use + web_search_tool_result(with citation) + text
    let mut p = SseParser::new();

    // index 0: text block
    feed_string(
        &mut p,
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
    );
    let t1 = feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Searching...\"}}\n\n",
    );
    assert!(matches!(t1.as_slice(), [Ok(ProviderEvent::TextDelta(s))] if s == "Searching..."));
    feed_string(
        &mut p,
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    );

    // index 1: server_tool_use
    feed_string(
        &mut p,
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"server_tool_use\",\"id\":\"s1\",\"name\":\"web_search_20250305\"}}\n\n",
    );
    feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\":\\\"rust tokio\\\"}\" }}\n\n",
    );
    let stop1 = feed_string(
        &mut p,
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
    );
    assert!(stop1.is_empty(), "server_tool_use stop should emit nothing");

    // index 2: web_search_tool_result with citation
    let result_block = serde_json::json!({
        "type": "web_search_tool_result",
        "tool_use_id": "s1",
        "content": [
            {"type": "web_search_result", "url": "https://tokio.rs", "title": "Tokio", "encrypted_content": "enc_xyz"},
            {
                "type": "text",
                "text": "Tokio is async runtime",
                "citations": [{
                    "type": "web_search_result_location",
                    "url": "https://tokio.rs",
                    "title": "Tokio",
                    "cited_text": "async runtime"
                }]
            }
        ]
    });
    let start2 =
        serde_json::json!({"type": "content_block_start","index": 2,"content_block": result_block});
    feed_string(
        &mut p,
        &format!("event: content_block_start\ndata: {start2}\n\n"),
    );
    let res = feed_string(
        &mut p,
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
    );
    assert_eq!(res.len(), 1);
    match &res[0] {
        Ok(ProviderEvent::ServerToolResult {
            provider,
            name,
            content,
            citations,
        }) => {
            assert_eq!(provider, "anthropic");
            assert_eq!(name, "web_search");
            assert_eq!(content["content"][0]["encrypted_content"], "enc_xyz");
            assert_eq!(citations.len(), 1);
            assert_eq!(citations[0].url, "https://tokio.rs");
        }
        other => panic!("unexpected: {other:?}"),
    }

    // index 3: follow-up text block
    feed_string(
        &mut p,
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":3,\"content_block\":{\"type\":\"text\"}}\n\n",
    );
    let t2 = feed_string(
        &mut p,
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":3,\"delta\":{\"type\":\"text_delta\",\"text\":\"Based on results...\"}}\n\n",
    );
    assert!(
        matches!(t2.as_slice(), [Ok(ProviderEvent::TextDelta(s))] if s == "Based on results...")
    );
}
