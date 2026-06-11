mod common;

use std::sync::{Arc, Mutex};

use crabgent_core::{AudioPayload, SttError, SttModelId, SttProvider, SttRequest};
use crabgent_provider_elevenlabs::{ElevenLabsSttProvider, SttWsClient};
use mockito::Matcher;
use serde_json::json;

use crate::common::stt_test_ctx;

const XI_API_KEY: &str = "secret-test-xi-key-99999";

fn audio_request() -> SttRequest {
    SttRequest {
        payload: AudioPayload::new(
            b"RIFF\0\0\0\0WAVE".to_vec(),
            "audio/wav",
            Some("clip.wav".to_owned()),
        )
        .expect("valid audio payload"),
        model: SttModelId::new("scribe_v2"),
        language: Some("en".to_owned()),
    }
}

fn audio_request_without_optional_fields() -> SttRequest {
    SttRequest {
        payload: AudioPayload::new(b"RIFF\0\0\0\0WAVE".to_vec(), "audio/wav", None)
            .expect("valid audio payload"),
        model: SttModelId::new("scribe_v2"),
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
async fn xi_api_key_header_sent() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let mock = server
        .mock("POST", "/v1/speech-to-text")
        .match_header("xi-api-key", Matcher::Exact(XI_API_KEY.to_owned()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(json!({"text": "hello"}).to_string())
        .expect(1)
        .create_async()
        .await;

    let response = provider(&ctx)
        .transcribe(audio_request())
        .await
        .expect("batch response");

    assert_eq!(response.text, "hello");
    mock.assert_async().await;
}

#[tokio::test]
async fn multipart_model_id_scribe_v2_field_present() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&bodies);
    let mock = server
        .mock("POST", "/v1/speech-to-text")
        .match_header(
            "content-type",
            Matcher::Regex("multipart/form-data.*".into()),
        )
        .match_request(move |request| {
            let body = request
                .utf8_lossy_body()
                .expect("multipart body")
                .into_owned();
            recorder.lock().expect("body lock").push(body);
            true
        })
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(json!({"text": "hello multipart"}).to_string())
        .expect(1)
        .create_async()
        .await;

    let response = provider(&ctx)
        .transcribe(audio_request())
        .await
        .expect("batch response");

    assert_eq!(response.text, "hello multipart");
    mock.assert_async().await;
    let body = bodies.lock().expect("body lock").join("\n");
    assert!(body.contains("name=\"file\""));
    assert!(body.contains("filename=\"clip.wav\""));
    assert!(body.contains("name=\"model_id\""));
    assert!(body.contains("scribe_v2"));
    assert!(body.contains("name=\"language_code\""));
    assert!(body.contains("en"));
}

#[tokio::test]
async fn parses_response() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let mock = server
        .mock("POST", "/v1/speech-to-text")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(json!({"text": "parsed text"}).to_string())
        .expect(1)
        .create_async()
        .await;

    let response = provider(&ctx)
        .transcribe(audio_request())
        .await
        .expect("batch response");

    assert_eq!(response.text, "parsed text");
    assert_eq!(response.model.as_str(), "scribe_v2");
    assert!(response.segments.is_empty());
    mock.assert_async().await;
}

#[tokio::test]
async fn parses_words_audio_events_and_language() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let body = json!({
        "text": "Hey world",
        "language_code": "en",
        "language_probability": 0.98,
        "words": [
            {"text": "Hey", "start": 0.0, "end": 0.4, "type": "word", "speaker_id": "speaker_0"},
            {"text": "(laughter)", "start": 0.4, "end": 0.9, "type": "audio_event"},
            {"text": " ", "type": "spacing"},
            {"text": "world", "start": 0.9, "end": 1.3, "type": "word", "speaker_id": "speaker_1"}
        ]
    });
    let mock = server
        .mock("POST", "/v1/speech-to-text")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body.to_string())
        .expect(1)
        .create_async()
        .await;

    let response = provider(&ctx)
        .transcribe(audio_request())
        .await
        .expect("batch response");

    assert_eq!(response.segments.len(), 1);
    let segment = response.segments.first().expect("one segment");
    assert_eq!(segment.words.len(), 2);
    assert_eq!(segment.speaker_id, None);
    assert_eq!(segment.words[0].text, "Hey");
    assert_eq!(segment.words[1].text, "world");
    assert_eq!(segment.words[0].speaker_id.as_deref(), Some("speaker_0"));
    assert_eq!(segment.words[1].speaker_id.as_deref(), Some("speaker_1"));
    assert!((segment.words[0].start - 0.0).abs() < f32::EPSILON);
    assert!((segment.words[1].end - 1.3).abs() < f32::EPSILON);
    assert_eq!(response.audio_events.len(), 1);
    let event = response.audio_events.first().expect("one audio event");
    assert_eq!(event.label, "(laughter)");
    assert_eq!(event.start_ms, Some(400));
    assert_eq!(response.language, Some("en".to_owned()));
    mock.assert_async().await;
}

#[tokio::test]
async fn null_word_timestamps_are_not_fabricated() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let body = json!({
        "text": "ok then",
        "words": [
            {"text": "ok", "start": 0.0, "end": 0.3, "type": "word"},
            {"text": "then", "type": "word"}
        ]
    });
    let mock = server
        .mock("POST", "/v1/speech-to-text")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body.to_string())
        .expect(1)
        .create_async()
        .await;

    let response = provider(&ctx)
        .transcribe(audio_request())
        .await
        .expect("batch response");

    assert_eq!(response.text, "ok then");
    assert_eq!(response.segments.len(), 1);
    let segment = response.segments.first().expect("one segment");
    assert_eq!(segment.words.len(), 1);
    assert_eq!(segment.words[0].text, "ok");
    assert!(
        !segment.words.iter().any(|word| word.text == "then"),
        "untimed word must not be fabricated into the timing list"
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn form_includes_timestamps_audio_events_and_diarize() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&bodies);
    let mock = server
        .mock("POST", "/v1/speech-to-text")
        .match_request(move |request| {
            let body = request
                .utf8_lossy_body()
                .expect("multipart body")
                .into_owned();
            recorder.lock().expect("body lock").push(body);
            true
        })
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(json!({"text": "granularity check"}).to_string())
        .expect(1)
        .create_async()
        .await;

    let response = provider(&ctx)
        .transcribe(audio_request())
        .await
        .expect("batch response");

    assert_eq!(response.text, "granularity check");
    mock.assert_async().await;
    let body = bodies.lock().expect("body lock").join("\n");
    assert!(body.contains("name=\"timestamps_granularity\""));
    assert!(body.contains("word"));
    assert!(body.contains("name=\"tag_audio_events\""));
    assert!(body.contains("true"));
    assert!(body.contains("name=\"diarize\""));
    assert!(body.contains("true"));
}

#[tokio::test]
async fn default_filename_and_no_language_are_accepted() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&bodies);
    let mock = server
        .mock("POST", "/v1/speech-to-text")
        .match_request(move |request| {
            let body = request
                .utf8_lossy_body()
                .expect("multipart body")
                .into_owned();
            recorder.lock().expect("body lock").push(body);
            true
        })
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(json!({"text": "no optional fields"}).to_string())
        .expect(1)
        .create_async()
        .await;

    let response = provider(&ctx)
        .transcribe(audio_request_without_optional_fields())
        .await
        .expect("batch response");

    assert_eq!(response.text, "no optional fields");
    mock.assert_async().await;
    let body = bodies.lock().expect("body lock").join("\n");
    assert!(body.contains("filename=\"audio\""));
    assert!(!body.contains("language_code"));
}

#[tokio::test]
async fn backend_failure_maps_to_backend_error() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let mock = server
        .mock("POST", "/v1/speech-to-text")
        .with_status(500)
        .with_body("server unavailable")
        .expect(1)
        .create_async()
        .await;

    let err = provider(&ctx)
        .transcribe(audio_request())
        .await
        .expect_err("backend failure");

    assert!(matches!(err, SttError::Backend(_)));
    mock.assert_async().await;
}

#[tokio::test]
async fn backend_failure_does_not_return_body_excerpt() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let mock = server
        .mock("POST", "/v1/speech-to-text")
        .with_status(500)
        .with_body("backend spilled private upstream detail")
        .expect(1)
        .create_async()
        .await;

    let err = provider(&ctx)
        .transcribe(audio_request())
        .await
        .expect_err("backend failure");

    let rendered = format!("{err:?}\n{err}");
    assert!(
        !rendered.contains("private upstream detail"),
        "backend body leaked: {rendered}"
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn invalid_response_json_maps_to_decode_error() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let mock = server
        .mock("POST", "/v1/speech-to-text")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{not json")
        .expect(1)
        .create_async()
        .await;

    let err = provider(&ctx)
        .transcribe(audio_request())
        .await
        .expect_err("decode failure");

    assert!(matches!(err, SttError::Decode));
    mock.assert_async().await;
}

#[tokio::test]
async fn audio_payload_rejects_invalid_mime_before_provider() {
    let err = AudioPayload::new(vec![1, 2, 3], "not a mime", Some("clip.wav".to_owned()))
        .expect_err("mime rejected");

    assert!(
        err.to_string()
            .contains("unsupported MIME type: not a mime")
    );
}

#[tokio::test]
async fn auth_failure_no_key_leak() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let mock = server
        .mock("POST", "/v1/speech-to-text")
        .with_status(401)
        .with_body(format!("bad key {XI_API_KEY}"))
        .expect(1)
        .create_async()
        .await;

    let err = provider(&ctx)
        .transcribe(audio_request())
        .await
        .expect_err("auth failure");

    let rendered = format!("{err:?}\n{err}");
    assert!(!rendered.contains(XI_API_KEY), "secret leaked: {rendered}");
    assert!(!rendered.contains("401"), "status leaked: {rendered}");
    mock.assert_async().await;
}
