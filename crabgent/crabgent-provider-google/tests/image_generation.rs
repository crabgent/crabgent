use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use crabgent_core::{
    ImageGenerationAspectRatio, ImageGenerationError, ImageGenerationProvider,
    ImageGenerationRequest, ImageGenerationSize, RunCtx, RunId, Subject,
};
use crabgent_provider_google::models::GEMINI_3_1_FLASH_IMAGE_PREVIEW;
use crabgent_provider_google::{GoogleConfig, GoogleImageGenerationProvider};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

const API_KEY_SECRET: &str = "secret-test-google-key-99999";

fn config(base_url: &str) -> GoogleConfig {
    GoogleConfig::new(API_KEY_SECRET.to_owned())
        .with_base_url(base_url.to_owned())
        .with_max_retries(0)
        .with_retry_base_delay(Duration::from_millis(1))
        .with_request_timeout(Duration::from_secs(2))
}

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("test-subject"))
}

#[tokio::test]
async fn gemini_image_generation_parses_inline_data() {
    let mut server = mockito::Server::new_async().await;
    let png = BASE64_STANDARD.encode(png_header());
    let mock = server
        .mock(
            "POST",
            "/v1beta/models/gemini-3.1-flash-image-preview:generateContent",
        )
        .match_header("x-goog-api-key", API_KEY_SECRET)
        .match_request(assert_image_body)
        .with_status(200)
        .with_body(format!(
            r#"{{
                "candidates": [{{
                    "content": {{"parts": [
                        {{"text": "done"}},
                        {{"inlineData": {{"mimeType": "image/png", "data": "{png}"}}}}
                    ]}}
                }}],
                "usageMetadata": {{
                    "promptTokenCount": 3,
                    "candidatesTokenCount": 5,
                    "totalTokenCount": 8
                }}
            }}"#
        ))
        .expect(1)
        .create_async()
        .await;
    let provider =
        GoogleImageGenerationProvider::try_new(reqwest::Client::new(), config(&server.url()))
            .expect("valid image provider");
    let mut req = ImageGenerationRequest::new(GEMINI_3_1_FLASH_IMAGE_PREVIEW, "draw a cube");
    req.aspect_ratio = Some(ImageGenerationAspectRatio::new("1:1"));

    let response = provider
        .generate_image(req, &ctx(), None)
        .await
        .expect("image response");

    mock.assert_async().await;
    assert_eq!(response.text.as_deref(), Some("done"));
    assert_eq!(response.images.len(), 1);
    assert_eq!(response.images[0].mime(), "image/png");
    assert_eq!(response.usage.expect("usage").total_tokens, 8);
}

#[tokio::test]
async fn gemini_image_generation_backend_error_is_opaque() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock(
            "POST",
            "/v1beta/models/gemini-3.1-flash-image-preview:generateContent",
        )
        .with_status(500)
        .with_body(format!("backend body {API_KEY_SECRET}"))
        .expect(1)
        .create_async()
        .await;
    let provider =
        GoogleImageGenerationProvider::try_new(reqwest::Client::new(), config(&server.url()))
            .expect("valid image provider");

    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GEMINI_3_1_FLASH_IMAGE_PREVIEW, "draw a cube"),
            &ctx(),
            None,
        )
        .await
        .expect_err("backend error");

    mock.assert_async().await;
    assert!(matches!(err, ImageGenerationError::Backend(_)));
    let rendered = format!("{err:?}\n{err}");
    assert!(
        !rendered.contains(API_KEY_SECRET),
        "secret leaked: {rendered}"
    );
}

#[tokio::test]
async fn gemini_image_generation_backend_with_large_body_still_maps_to_backend() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock(
            "POST",
            "/v1beta/models/gemini-3.1-flash-image-preview:generateContent",
        )
        .with_status(500)
        .with_body("x".repeat(70_000))
        .expect(1)
        .create_async()
        .await;
    let provider =
        GoogleImageGenerationProvider::try_new(reqwest::Client::new(), config(&server.url()))
            .expect("valid image provider");

    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GEMINI_3_1_FLASH_IMAGE_PREVIEW, "draw a cube"),
            &ctx(),
            None,
        )
        .await
        .expect_err("backend error");

    mock.assert_async().await;
    assert!(matches!(err, ImageGenerationError::Backend(_)));
}

#[tokio::test]
async fn gemini_image_generation_rejects_unsupported_size() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock(
            "POST",
            "/v1beta/models/gemini-3.1-flash-image-preview:generateContent",
        )
        .expect(0)
        .create_async()
        .await;
    let provider =
        GoogleImageGenerationProvider::try_new(reqwest::Client::new(), config(&server.url()))
            .expect("valid image provider");
    let mut req = ImageGenerationRequest::new(GEMINI_3_1_FLASH_IMAGE_PREVIEW, "draw a cube");
    req.size = Some(ImageGenerationSize::new("1024x1024"));

    let err = provider
        .generate_image(req, &ctx(), None)
        .await
        .expect_err("unsupported size");

    mock.assert_async().await;
    assert!(matches!(
        err,
        ImageGenerationError::UnsupportedOption { option, .. } if option == "size"
    ));
}

#[tokio::test]
async fn gemini_image_generation_rejects_missing_inline_data() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock(
            "POST",
            "/v1beta/models/gemini-3.1-flash-image-preview:generateContent",
        )
        .with_status(200)
        .with_body(
            r#"{
                "candidates": [{
                    "content": {"parts": [{"text": "no image"}]}
                }]
            }"#,
        )
        .expect(1)
        .create_async()
        .await;
    let provider =
        GoogleImageGenerationProvider::try_new(reqwest::Client::new(), config(&server.url()))
            .expect("valid image provider");

    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GEMINI_3_1_FLASH_IMAGE_PREVIEW, "draw a cube"),
            &ctx(),
            None,
        )
        .await
        .expect_err("decode error");

    mock.assert_async().await;
    let ImageGenerationError::Decode(message) = err else {
        panic!("unexpected error");
    };
    assert!(message.contains("inlineData"));
}

#[tokio::test]
async fn gemini_image_generation_rejects_invalid_base64() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock(
            "POST",
            "/v1beta/models/gemini-3.1-flash-image-preview:generateContent",
        )
        .with_status(200)
        .with_body(
            r#"{
                "candidates": [{
                    "content": {"parts": [
                        {"inlineData": {"mimeType": "image/png", "data": "not base64"}}
                    ]}
                }]
            }"#,
        )
        .expect(1)
        .create_async()
        .await;
    let provider =
        GoogleImageGenerationProvider::try_new(reqwest::Client::new(), config(&server.url()))
            .expect("valid image provider");

    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GEMINI_3_1_FLASH_IMAGE_PREVIEW, "draw a cube"),
            &ctx(),
            None,
        )
        .await
        .expect_err("decode error");

    mock.assert_async().await;
    assert!(matches!(err, ImageGenerationError::Decode(_)));
}

#[tokio::test]
async fn gemini_image_generation_cancelled_before_http_call() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock(
            "POST",
            "/v1beta/models/gemini-3.1-flash-image-preview:generateContent",
        )
        .expect(0)
        .create_async()
        .await;
    let provider =
        GoogleImageGenerationProvider::try_new(reqwest::Client::new(), config(&server.url()))
            .expect("valid image provider");
    let cancel = CancellationToken::new();
    cancel.cancel();

    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GEMINI_3_1_FLASH_IMAGE_PREVIEW, "draw a cube"),
            &ctx(),
            Some(&cancel),
        )
        .await
        .expect_err("cancelled");

    mock.assert_async().await;
    assert!(matches!(err, ImageGenerationError::Cancelled));
}

fn assert_image_body(request: &mockito::Request) -> bool {
    let Some(value) = body_json(request) else {
        return false;
    };
    value["contents"][0]["parts"][0]["text"] == "draw a cube"
        && value["generationConfig"]["responseModalities"][0] == "TEXT"
        && value["generationConfig"]["responseModalities"][1] == "IMAGE"
        && value["generationConfig"]["imageConfig"]["aspectRatio"] == "1:1"
}

fn body_json(request: &mockito::Request) -> Option<Value> {
    let body = request.utf8_lossy_body().ok()?;
    serde_json::from_str::<Value>(&body).ok()
}

fn png_header() -> Vec<u8> {
    b"\x89PNG\r\n\x1a\n".to_vec()
}
