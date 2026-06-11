mod common;

use std::sync::{Arc, Mutex};

use crabgent_core::{AudioPayload, SttModelId, SttProvider, SttRequest};
use crabgent_provider_openai::OpenAiSttProvider;
use mockito::Matcher;
use serde_json::json;

use crate::common::stt_test_ctx;

const API_KEY_SECRET: &str = "secret-test-openai-stt-key-99999";

fn audio_request() -> SttRequest {
    SttRequest {
        payload: AudioPayload::new(
            b"OggS\0\0\0\0".to_vec(),
            "audio/ogg",
            Some("clip.ogg".to_owned()),
        )
        .expect("valid audio payload"),
        model: SttModelId::new("gpt-4o-transcribe"),
        language: Some("en".to_owned()),
    }
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
async fn apikey_bearer_header_sent() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let mock = server
        .mock("POST", "/v1/audio/transcriptions")
        .match_header(
            "authorization",
            Matcher::Exact(format!("Bearer {API_KEY_SECRET}")),
        )
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
async fn multipart_body_has_file_model_response_format() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&bodies);
    let mock = server
        .mock("POST", "/v1/audio/transcriptions")
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
    // build_form() sanitises user-supplied filenames to `audio.<ext>`
    // derived from the MIME type: the Codex transcribe endpoint 500s on
    // colons / non-ASCII in filenames, see commit 864e221.
    assert!(body.contains("filename=\"audio.ogg\""));
    assert!(body.contains("name=\"model\""));
    assert!(body.contains("gpt-4o-transcribe"));
    assert!(body.contains("name=\"response_format\""));
    assert!(body.contains("json"));
    assert!(body.contains("name=\"language\""));
    assert!(body.contains("en"));
}

#[tokio::test]
async fn parses_json_response() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let mock = server
        .mock("POST", "/v1/audio/transcriptions")
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
    assert_eq!(response.model.as_str(), "gpt-4o-transcribe");
    assert!(response.segments.is_empty());
    mock.assert_async().await;
}

#[tokio::test]
async fn auth_failure_no_token_leak_stt() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let mock = server
        .mock("POST", "/v1/audio/transcriptions")
        .with_status(401)
        .with_body(format!("bad key {API_KEY_SECRET}"))
        .expect(1)
        .create_async()
        .await;

    let err = provider(&ctx)
        .transcribe(audio_request())
        .await
        .expect_err("auth failure");

    let rendered = format!("{err:?}\n{err}");
    assert!(
        !rendered.contains(API_KEY_SECRET),
        "secret leaked: {rendered}"
    );
    assert!(!rendered.contains("401"), "status leaked: {rendered}");
    mock.assert_async().await;
}

#[tokio::test]
async fn backend_failure_does_not_return_body_excerpt() {
    let mut ctx = stt_test_ctx().await;
    let Some(server) = ctx.batch_server.as_mut() else {
        return;
    };
    let mock = server
        .mock("POST", "/v1/audio/transcriptions")
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
    assert!(
        rendered.contains("500"),
        "backend status should remain diagnosable: {rendered}"
    );
    mock.assert_async().await;
}
