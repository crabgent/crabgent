mod common;

use std::sync::Arc;

use crabgent_core::{AudioPayload, SttEvent, SttModelId, SttProvider, SttRequest};
use crabgent_provider_elevenlabs::{ElevenLabsConfig, ElevenLabsSttProvider, SttWsClient};
use futures::StreamExt;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};

use crate::common::stt_test_ctx;

fn audio_request() -> SttRequest {
    SttRequest {
        payload: AudioPayload::new(vec![1, 2, 3, 4], "audio/wav", Some("clip.wav".to_owned()))
            .expect("valid audio payload"),
        model: SttModelId::new("scribe_v2_realtime"),
        language: None,
    }
}

fn empty_audio_request() -> SttRequest {
    SttRequest {
        payload: AudioPayload::new(Vec::<u8>::new(), "audio/wav", Some("empty.wav".to_owned()))
            .expect("valid audio payload"),
        model: SttModelId::new("scribe_v2_realtime"),
        language: None,
    }
}

fn large_audio_request() -> SttRequest {
    SttRequest {
        payload: AudioPayload::new(
            vec![7; (64 * 1024) + 1],
            "audio/wav",
            Some("large.wav".to_owned()),
        )
        .expect("valid audio payload"),
        model: SttModelId::new("scribe_v2_realtime"),
        language: None,
    }
}

fn provider(ctx: &common::SttTestCtx) -> ElevenLabsSttProvider {
    let ws_client: Arc<dyn SttWsClient> = ctx.ws_client.clone();
    ElevenLabsSttProvider::try_new(
        reqwest::Client::new(),
        Arc::new(ctx.config.clone()),
        ws_client,
    )
    .expect("valid STT provider")
}

#[tokio::test]
async fn session_started_event_consumed_silently() {
    let ctx = stt_test_ctx().await;
    ctx.ws_client.push_inbound_text(
        json!({
            "message_type": "session_started",
            "session_id": "test-session"
        })
        .to_string(),
    );
    ctx.ws_client.push_inbound_text(
        json!({
            "message_type": "partial_transcript",
            "text": "hel"
        })
        .to_string(),
    );
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    let event = stream.next().await.expect("one event").expect("delta");
    assert_eq!(event, SttEvent::Delta("hel".to_owned()));

    let urls = ctx.ws_client.connected_urls();
    assert_eq!(urls.len(), 1);
    assert!(urls[0].starts_with("ws://"));
    assert!(urls[0].contains("/v1/speech-to-text/realtime?model_id=scribe_v2_realtime"));
    let headers = ctx.ws_client.connected_headers();
    assert!(headers[0].contains(&(
        "xi-api-key".to_owned(),
        "secret-test-xi-key-99999".to_owned()
    )));
    let sent = ctx.ws_client.sent_texts();
    let chunk: serde_json::Value = serde_json::from_str(&sent[0]).expect("chunk JSON");
    assert_eq!(chunk["message_type"], "input_audio_chunk");
    assert_eq!(chunk["sample_rate"], 16_000);
    assert_eq!(chunk["commit"], true);
}

#[tokio::test]
async fn each_stream_uses_isolated_ws_session() {
    let ctx = stt_test_ctx().await;
    let provider = provider(&ctx);

    let _first = provider
        .stream(audio_request())
        .await
        .expect("first stream opened");
    let _second = provider
        .stream(audio_request())
        .await
        .expect("second stream opened");

    assert_eq!(ctx.ws_client.session_count(), 2);
    assert_eq!(ctx.ws_client.connected_urls().len(), 2);
}

#[tokio::test]
async fn partial_transcript_yields_delta() {
    let ctx = stt_test_ctx().await;
    ctx.ws_client.push_inbound_text(
        json!({
            "message_type": "partial_transcript",
            "text": "partial"
        })
        .to_string(),
    );
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    let event = stream.next().await.expect("one event").expect("delta");
    assert_eq!(event, SttEvent::Delta("partial".to_owned()));
}

#[tokio::test]
async fn committed_transcript_yields_final() {
    let ctx = stt_test_ctx().await;
    ctx.ws_client.push_inbound_text(
        json!({
            "message_type": "committed_transcript",
            "text": "final text"
        })
        .to_string(),
    );
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    let event = stream.next().await.expect("one event").expect("final");
    match event {
        SttEvent::Final(response) => {
            assert_eq!(response.text, "final text");
            assert_eq!(response.model.as_str(), "scribe_v2_realtime");
        }
        other => panic!("expected final event, got {other:?}"),
    }
    assert!(ctx.ws_client.closed());
}

#[tokio::test]
async fn realtime_with_timestamps_populates_words_and_language() {
    let ctx = stt_test_ctx().await;
    ctx.ws_client.push_inbound_text(
        json!({
            "message_type": "committed_transcript_with_timestamps",
            "text": "ja super",
            "language_code": "de",
            "words": [
                {"text": "ja", "start": 0.0, "end": 0.3, "type": "word"},
                {"text": "super", "start": 0.5, "end": 0.9, "type": "word"}
            ]
        })
        .to_string(),
    );
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    let event = stream.next().await.expect("one event").expect("final");
    match event {
        SttEvent::Final(response) => {
            assert_eq!(response.text, "ja super");
            assert_eq!(response.language, Some("de".to_owned()));
            assert_eq!(response.segments.len(), 1);
            let segment = response.segments.first().expect("one segment");
            assert_eq!(segment.words.len(), 2);
            assert_eq!(segment.text, "ja super");
            assert_eq!(segment.confidence, None);
            assert!((segment.start - 0.0).abs() < f32::EPSILON);
            assert!((segment.end - 0.9).abs() < f32::EPSILON);
            assert_eq!(segment.words[0].text, "ja");
            assert!((segment.words[0].start - 0.0).abs() < f32::EPSILON);
            assert!((segment.words[1].end - 0.9).abs() < f32::EPSILON);
            assert!(response.audio_events.is_empty());
        }
        other => panic!("expected final event, got {other:?}"),
    }
    assert!(ctx.ws_client.closed());
}

#[tokio::test]
async fn realtime_plain_committed_has_no_word_fabrication() {
    let ctx = stt_test_ctx().await;
    ctx.ws_client.push_inbound_text(
        json!({
            "message_type": "committed_transcript",
            "text": "ja super"
        })
        .to_string(),
    );
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    let event = stream.next().await.expect("one event").expect("final");
    match event {
        SttEvent::Final(response) => {
            assert_eq!(response.text, "ja super");
            assert!(response.segments.is_empty());
            assert!(response.audio_events.is_empty());
            assert_eq!(response.language, None);
        }
        other => panic!("expected final event, got {other:?}"),
    }
}

#[tokio::test]
async fn realtime_url_requests_timestamps() {
    let ctx = stt_test_ctx().await;
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    assert!(stream.next().await.is_none());
    let urls = ctx.ws_client.connected_urls();
    assert!(
        urls[0].contains("?model_id=scribe_v2_realtime&include_timestamps=true"),
        "url missing include_timestamps query: {}",
        urls[0]
    );
}

#[tokio::test]
async fn transcript_field_and_model_id_are_used_for_final() {
    let ctx = stt_test_ctx().await;
    ctx.ws_client.push_inbound_text(
        json!({
            "message_type": "committed_transcript",
            "transcript": "transcript text",
            "model_id": "custom_stt"
        })
        .to_string(),
    );
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    let event = stream.next().await.expect("one event").expect("final");
    match event {
        SttEvent::Final(response) => {
            assert_eq!(response.text, "transcript text");
            assert_eq!(response.model.as_str(), "custom_stt");
        }
        other => panic!("expected final event, got {other:?}"),
    }
}

#[tokio::test]
async fn empty_final_uses_accumulated_delta_text() {
    let ctx = stt_test_ctx().await;
    ctx.ws_client.push_inbound_text(
        json!({
            "message_type": "partial_transcript",
            "text": "accumulated "
        })
        .to_string(),
    );
    ctx.ws_client.push_inbound_text(
        json!({
            "message_type": "committed_transcript"
        })
        .to_string(),
    );
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    let first = stream.next().await.expect("delta").expect("delta");
    assert_eq!(first, SttEvent::Delta("accumulated ".to_owned()));
    let second = stream.next().await.expect("final").expect("final");
    match second {
        SttEvent::Final(response) => assert_eq!(response.text, "accumulated "),
        other => panic!("expected final event, got {other:?}"),
    }
}

#[tokio::test]
async fn binary_error_and_close_events_map_to_errors() {
    let ctx = stt_test_ctx().await;
    ctx.ws_client.push_inbound_message(Message::Binary(
        json!({"message_type": "error"})
            .to_string()
            .into_bytes()
            .into(),
    ));
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    let event = stream.next().await.expect("error event").expect("ok event");
    assert!(matches!(
        event,
        SttEvent::Error(crabgent_core::SttError::Backend(_))
    ));

    let ctx = stt_test_ctx().await;
    ctx.ws_client
        .push_inbound_message(Message::Close(Some(CloseFrame {
            code: CloseCode::Library(4401),
            reason: "auth".into(),
        })));
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");
    let event = stream.next().await.expect("error event").expect("ok event");
    assert!(matches!(
        event,
        SttEvent::Error(crabgent_core::SttError::Auth(_))
    ));
}

#[tokio::test]
async fn empty_audio_sends_commit_chunk() {
    let ctx = stt_test_ctx().await;
    let mut stream = provider(&ctx)
        .stream(empty_audio_request())
        .await
        .expect("stream opened");

    assert!(stream.next().await.is_none());
    let sent = ctx.ws_client.sent_texts();
    assert_eq!(sent.len(), 1);
    let chunk: serde_json::Value = serde_json::from_str(&sent[0]).expect("chunk JSON");
    assert_eq!(chunk["audio_base_64"], "");
    assert_eq!(chunk["commit"], true);
}

#[tokio::test]
async fn large_audio_splits_into_multiple_chunks() {
    let ctx = stt_test_ctx().await;
    let mut stream = provider(&ctx)
        .stream(large_audio_request())
        .await
        .expect("stream opened");

    assert!(stream.next().await.is_none());
    let sent = ctx.ws_client.sent_texts();
    assert_eq!(sent.len(), 2);
    let first: serde_json::Value = serde_json::from_str(&sent[0]).expect("first chunk JSON");
    let second: serde_json::Value = serde_json::from_str(&sent[1]).expect("second chunk JSON");
    assert_eq!(first["commit"], false);
    assert_eq!(second["commit"], true);
    assert!(first["audio_base_64"].as_str().expect("audio").len() > 10);
    assert!(second["audio_base_64"].as_str().expect("audio").len() > 1);
}

#[tokio::test]
async fn https_api_base_maps_to_wss_realtime_url() {
    let ctx = stt_test_ctx().await;
    let ws_client: Arc<dyn SttWsClient> = ctx.ws_client.clone();
    let provider = ElevenLabsSttProvider::try_new(
        reqwest::Client::new(),
        Arc::new(
            ElevenLabsConfig::new("secret-test-xi-key-99999").with_api_base("https://example.test"),
        ),
        ws_client,
    )
    .expect("valid STT provider");
    let mut stream = provider
        .stream(audio_request())
        .await
        .expect("stream opened");

    assert!(stream.next().await.is_none());
    let urls = ctx.ws_client.connected_urls();
    assert_eq!(
        urls[0],
        "wss://example.test/v1/speech-to-text/realtime?model_id=scribe_v2_realtime&include_timestamps=true"
    );
}
