mod common;

use std::sync::Arc;

use crabgent_core::{AudioPayload, SttError, SttEvent, SttModelId, SttProvider, SttRequest};
use crabgent_provider_openai::OpenAiSttProvider;
use futures::StreamExt;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};

use crate::common::stt_test_ctx;

fn audio_request() -> SttRequest {
    SttRequest {
        payload: AudioPayload::new(
            wav_pcm16_24khz_mono(&[1, -2]),
            "audio/wav",
            Some("clip.wav".to_owned()),
        )
        .expect("valid audio payload"),
        model: SttModelId::new("gpt-realtime-whisper"),
        language: None,
    }
}

fn invalid_wav_request() -> SttRequest {
    SttRequest {
        payload: AudioPayload::new(vec![1, 2, 3, 4], "audio/wav", Some("bad.wav".to_owned()))
            .expect("valid audio payload"),
        model: SttModelId::new("gpt-realtime-whisper"),
        language: None,
    }
}

fn wav_pcm16_24khz_mono(samples: &[i16]) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(
        &u32::try_from(36 + data_len)
            .expect("small fixture")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&24_000_u32.to_le_bytes());
    bytes.extend_from_slice(&48_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(
        &u32::try_from(data_len)
            .expect("small fixture")
            .to_le_bytes(),
    );
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn provider(ctx: &common::SttTestCtx) -> OpenAiSttProvider {
    OpenAiSttProvider::try_new(
        reqwest::Client::new(),
        Arc::new(ctx.config.clone()),
        Arc::clone(&ctx.auth),
        ctx.ws_client.clone(),
    )
    .expect("valid STT provider")
}

#[tokio::test]
async fn session_update_sent_first() {
    let ctx = stt_test_ctx().await;
    let _stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    let sent = ctx.ws_client.sent_texts();
    let first: serde_json::Value = serde_json::from_str(&sent[0]).expect("session JSON");
    assert_eq!(first["type"], "session.update");
    assert_eq!(
        first["session"]["input_audio_transcription"]["model"],
        "gpt-realtime-whisper"
    );
    let urls = ctx.ws_client.connected_urls();
    assert_eq!(urls.len(), 1);
    assert!(urls[0].starts_with("ws://"));
    assert!(urls[0].ends_with("/v1/realtime?intent=transcription"));
}

#[tokio::test]
async fn realtime_extracts_pcm_from_wav_before_append() {
    let ctx = stt_test_ctx().await;
    let _stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    let sent = ctx.ws_client.sent_texts();
    let append: serde_json::Value = serde_json::from_str(&sent[1]).expect("append JSON");
    assert_eq!(append["type"], "input_audio_buffer.append");
    assert_eq!(append["audio"], "AQD+/w==");
}

#[tokio::test]
async fn realtime_rejects_invalid_wav_before_connecting() {
    let ctx = stt_test_ctx().await;

    let result = provider(&ctx).stream(invalid_wav_request()).await;
    let Err(err) = result else {
        panic!("invalid wav should fail before websocket connect");
    };

    assert!(matches!(err, SttError::Decode));
    assert_eq!(ctx.ws_client.session_count(), 0);
    assert!(ctx.ws_client.connected_urls().is_empty());
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
async fn parses_delta_event_returns_delta() {
    let ctx = stt_test_ctx().await;
    ctx.ws_client.push_inbound_text(
        json!({
            "type": "conversation.item.input_audio_transcription.delta",
            "delta": "hel"
        })
        .to_string(),
    );
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    let event = stream
        .next()
        .await
        .expect("one event")
        .expect("delta event");

    assert_eq!(event, SttEvent::Delta("hel".to_owned()));
}

#[tokio::test]
async fn auth_close_frame_maps_to_auth_error_event() {
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

    let event = stream.next().await.expect("one event").expect("stt event");
    assert!(matches!(event, SttEvent::Error(SttError::Auth(_))));
}

#[tokio::test]
async fn parses_completed_event_returns_final() {
    let ctx = stt_test_ctx().await;
    ctx.ws_client.push_inbound_text(
        json!({
            "type": "conversation.item.input_audio_transcription.completed",
            "transcript": "hello"
        })
        .to_string(),
    );
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    let event = stream
        .next()
        .await
        .expect("one event")
        .expect("final event");

    match event {
        SttEvent::Final(response) => {
            assert_eq!(response.text, "hello");
            assert_eq!(response.model.as_str(), "gpt-realtime-whisper");
        }
        other => panic!("expected final event, got {other:?}"),
    }
    assert!(ctx.ws_client.closed());
}

#[tokio::test]
async fn accumulates_text_across_deltas() {
    let ctx = stt_test_ctx().await;
    ctx.ws_client.push_inbound_text(
        json!({
            "type": "conversation.item.input_audio_transcription.delta",
            "delta": "hel"
        })
        .to_string(),
    );
    ctx.ws_client.push_inbound_text(
        json!({
            "type": "conversation.item.input_audio_transcription.delta",
            "delta": "lo"
        })
        .to_string(),
    );
    ctx.ws_client.push_inbound_text(
        json!({
            "type": "conversation.item.input_audio_transcription.completed"
        })
        .to_string(),
    );
    let mut stream = provider(&ctx)
        .stream(audio_request())
        .await
        .expect("stream opened");

    assert_eq!(
        stream.next().await.expect("delta").expect("delta"),
        SttEvent::Delta("hel".to_owned())
    );
    assert_eq!(
        stream.next().await.expect("delta").expect("delta"),
        SttEvent::Delta("lo".to_owned())
    );
    let event = stream.next().await.expect("final").expect("final");

    match event {
        SttEvent::Final(response) => assert_eq!(response.text, "hello"),
        other => panic!("expected final event, got {other:?}"),
    }
}
