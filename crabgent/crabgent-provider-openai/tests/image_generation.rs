use std::num::NonZeroU8;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use crabgent_core::{
    ImageGenerationAspectRatio, ImageGenerationBackground, ImageGenerationError,
    ImageGenerationFormat, ImageGenerationProvider, ImageGenerationQuality, ImageGenerationRequest,
    ImageGenerationSize, RunCtx, RunId, Subject,
};
use crabgent_provider_openai::image_generation::{
    GPT_IMAGE_1, GPT_IMAGE_1_5, GPT_IMAGE_1_MINI, GPT_IMAGE_2,
};
use crabgent_provider_openai::models::GPT_5_5;
use crabgent_provider_openai::{
    ApiKeyAuth, CodexOAuthAuth, OpenAiConfig, OpenAiImageGenerationProvider,
};
use secrecy::SecretString;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

const API_KEY_SECRET: &str = "secret-test-openai-image-key-99999";

fn config() -> OpenAiConfig {
    OpenAiConfig::new(API_KEY_SECRET.to_owned())
        .with_max_retries(0)
        .with_retry_base_delay(Duration::from_millis(1))
        .with_request_timeout(Duration::from_secs(2))
}

fn provider(base_url: &str) -> OpenAiImageGenerationProvider {
    let auth = ApiKeyAuth::new(SecretString::from(API_KEY_SECRET.to_owned()))
        .with_base_url(base_url.to_owned());
    OpenAiImageGenerationProvider::try_new(reqwest::Client::new(), config(), Box::new(auth))
        .expect("valid image provider")
}

fn codex_provider(base_url: &str) -> OpenAiImageGenerationProvider {
    let auth = CodexOAuthAuth::new(SecretString::from(API_KEY_SECRET.to_owned()), None)
        .with_base_url(base_url.to_owned());
    OpenAiImageGenerationProvider::try_new(reqwest::Client::new(), config(), Box::new(auth))
        .expect("valid codex image provider")
}

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("test-subject"))
}

#[test]
fn image_generation_provider_accessors_are_available() {
    let provider =
        OpenAiImageGenerationProvider::try_from_api_key(reqwest::Client::new(), config())
            .expect("valid image provider");

    assert_eq!(provider.config().max_retries, 0);
    assert_eq!(provider.auth().base_url(), "https://api.openai.com");
    assert!(!provider.image_generation_capabilities().editing);
}

#[test]
fn image_generation_catalog_lists_gpt_image_models() {
    let provider = provider("http://localhost");
    let models = provider.image_generation_models();
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();

    assert!(ids.contains(&GPT_IMAGE_2));
    assert!(ids.contains(&GPT_IMAGE_1_5));
    assert!(ids.contains(&GPT_IMAGE_1));
    assert!(ids.contains(&GPT_IMAGE_1_MINI));
    assert!(models.iter().all(|model| !model.supports_editing));
    assert!(provider.image_generation_capabilities().generation);
}

#[tokio::test]
async fn generate_image_posts_to_image_api() {
    let mut server = mockito::Server::new_async().await;
    let png = BASE64_STANDARD.encode(png_header());
    let mock = server
        .mock("POST", "/v1/images/generations")
        .match_header("authorization", format!("Bearer {API_KEY_SECRET}").as_str())
        .match_request(assert_image_body)
        .with_status(200)
        .with_body(format!(
            r#"{{
                "output_format": "png",
                "data": [{{"b64_json": "{png}", "revised_prompt": "a better cube"}}],
                "usage": {{"input_tokens": 11, "output_tokens": 22, "total_tokens": 33}}
            }}"#
        ))
        .expect(1)
        .create_async()
        .await;

    let provider = provider(&server.url());
    let mut req = ImageGenerationRequest::new(GPT_IMAGE_2, "draw a cube");
    req.count = NonZeroU8::new(2).expect("2 is non-zero");
    req.size = Some(ImageGenerationSize::new("1024x1024"));
    req.quality = Some(ImageGenerationQuality::High);
    req.format = Some(ImageGenerationFormat::Png);
    req.background = Some(ImageGenerationBackground::Transparent);
    let response = provider
        .generate_image(req, &ctx(), None)
        .await
        .expect("image generation response");

    mock.assert_async().await;
    assert_eq!(response.model.as_str(), GPT_IMAGE_2);
    assert_eq!(response.images.len(), 1);
    assert_eq!(response.images[0].mime(), "image/png");
    assert_eq!(
        response.images[0].revised_prompt.as_deref(),
        Some("a better cube")
    );
    assert_eq!(response.usage.expect("usage").total_tokens, 33);
}

#[tokio::test]
async fn codex_oauth_uses_hosted_image_generation_tool() {
    let mut server = mockito::Server::new_async().await;
    let png = BASE64_STANDARD.encode(png_header());
    let run_ctx = ctx();
    let scope = run_ctx.run_id.to_string();
    let mock = server
        .mock("POST", "/backend-api/codex/responses")
        .match_header("authorization", format!("Bearer {API_KEY_SECRET}").as_str())
        .match_header("session_id", scope.as_str())
        .match_header(
            "x-codex-window-id",
            crabgent_provider_openai::auth::CODEX_INSTALLATION_ID,
        )
        .match_request(assert_hosted_image_body)
        .with_status(200)
        .with_body(format!(
            "data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"image_generation_call\",\"id\":\"ig_1\",\"status\":\"completed\",\"revised_prompt\":\"a better cube\",\"result\":\"{png}\"}}}}\n\
             data: {{\"type\":\"response.completed\",\"response\":{{\"status\":\"completed\",\"usage\":{{\"input_tokens\":11,\"output_tokens\":22,\"total_tokens\":33}}}}}}\n\
             data: [DONE]\n"
        ))
        .expect(1)
        .create_async()
        .await;

    let provider = codex_provider(&server.url());
    let response = provider
        .generate_image(
            ImageGenerationRequest::new(GPT_IMAGE_2, "draw a cube"),
            &run_ctx,
            None,
        )
        .await
        .expect("image response");

    mock.assert_async().await;
    assert_eq!(response.model.as_str(), GPT_IMAGE_2);
    assert_eq!(response.images.len(), 1);
    assert_eq!(response.images[0].mime(), "image/png");
    assert_eq!(
        response.images[0].revised_prompt.as_deref(),
        Some("a better cube")
    );
    assert_eq!(response.usage.expect("usage").total_tokens, 33);
}

#[tokio::test]
async fn image_generation_auth_failure_no_token_leak() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/images/generations")
        .with_status(401)
        .with_body(format!("bad key {API_KEY_SECRET}"))
        .expect(1)
        .create_async()
        .await;

    let provider = provider(&server.url());
    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GPT_IMAGE_2, "draw a cube"),
            &ctx(),
            None,
        )
        .await
        .expect_err("auth error");

    mock.assert_async().await;
    assert!(matches!(err, ImageGenerationError::Auth(_)));
    let rendered = format!("{err:?}\n{err}");
    assert!(
        !rendered.contains(API_KEY_SECRET),
        "secret leaked: {rendered}"
    );
    assert!(!rendered.contains("401"), "status leaked: {rendered}");
}

#[tokio::test]
async fn image_generation_auth_with_large_body_still_maps_to_auth() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/images/generations")
        .with_status(401)
        .with_body("x".repeat(70_000))
        .expect(1)
        .create_async()
        .await;

    let provider = provider(&server.url());
    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GPT_IMAGE_2, "draw a cube"),
            &ctx(),
            None,
        )
        .await
        .expect_err("auth error");

    mock.assert_async().await;
    assert!(matches!(err, ImageGenerationError::Auth(_)));
}

#[tokio::test]
async fn image_generation_cancelled_before_http_call() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/images/generations")
        .expect(0)
        .create_async()
        .await;
    let provider = provider(&server.url());
    let cancel = CancellationToken::new();
    cancel.cancel();

    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GPT_IMAGE_2, "draw a cube"),
            &ctx(),
            Some(&cancel),
        )
        .await
        .expect_err("cancelled");

    mock.assert_async().await;
    assert!(matches!(err, ImageGenerationError::Cancelled));
}

#[tokio::test]
async fn image_generation_rejects_unsupported_aspect_ratio() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/images/generations")
        .expect(0)
        .create_async()
        .await;
    let provider = provider(&server.url());
    let mut req = ImageGenerationRequest::new(GPT_IMAGE_2, "draw a cube");
    req.aspect_ratio = Some(ImageGenerationAspectRatio::new("1:1"));

    let err = provider
        .generate_image(req, &ctx(), None)
        .await
        .expect_err("unsupported aspect ratio");

    mock.assert_async().await;
    assert!(matches!(
        err,
        ImageGenerationError::UnsupportedOption { option, .. } if option == "aspect_ratio"
    ));
}

#[tokio::test]
async fn image_generation_uses_request_format_when_response_omits_format() {
    let mut server = mockito::Server::new_async().await;
    let jpeg = BASE64_STANDARD.encode(jpeg_header());
    let mock = server
        .mock("POST", "/v1/images/generations")
        .with_status(200)
        .with_body(format!(r#"{{"data": [{{"b64_json": "{jpeg}"}}]}}"#))
        .expect(1)
        .create_async()
        .await;
    let provider = provider(&server.url());
    let mut req = ImageGenerationRequest::new(GPT_IMAGE_2, "draw a cube");
    req.format = Some(ImageGenerationFormat::Jpeg);

    let response = provider
        .generate_image(req, &ctx(), None)
        .await
        .expect("jpeg response");

    mock.assert_async().await;
    assert_eq!(response.images[0].mime(), "image/jpeg");
}

#[tokio::test]
async fn image_generation_rejects_invalid_base64() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/images/generations")
        .with_status(200)
        .with_body(r#"{"data": [{"b64_json": "not-base64%%"}]}"#)
        .expect(1)
        .create_async()
        .await;
    let provider = provider(&server.url());

    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GPT_IMAGE_2, "draw a cube"),
            &ctx(),
            None,
        )
        .await
        .expect_err("decode error");

    mock.assert_async().await;
    assert!(matches!(err, ImageGenerationError::Decode(_)));
}

#[tokio::test]
async fn image_generation_rejects_missing_b64_json() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/images/generations")
        .with_status(200)
        .with_body(r#"{"data": [{}]}"#)
        .expect(1)
        .create_async()
        .await;
    let provider = provider(&server.url());

    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GPT_IMAGE_2, "draw a cube"),
            &ctx(),
            None,
        )
        .await
        .expect_err("decode error");

    mock.assert_async().await;
    assert!(err.to_string().contains("b64_json"));
}

#[tokio::test]
async fn image_generation_rejects_empty_image_data() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/images/generations")
        .with_status(200)
        .with_body(r#"{"data": []}"#)
        .expect(1)
        .create_async()
        .await;
    let provider = provider(&server.url());

    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GPT_IMAGE_2, "draw a cube"),
            &ctx(),
            None,
        )
        .await
        .expect_err("decode error");

    mock.assert_async().await;
    assert!(err.to_string().contains("any images"));
}

#[tokio::test]
async fn image_generation_rejects_unknown_output_format() {
    let mut server = mockito::Server::new_async().await;
    let png = BASE64_STANDARD.encode(png_header());
    let mock = server
        .mock("POST", "/v1/images/generations")
        .with_status(200)
        .with_body(format!(
            r#"{{"output_format": "bmp", "data": [{{"b64_json": "{png}"}}]}}"#
        ))
        .expect(1)
        .create_async()
        .await;
    let provider = provider(&server.url());

    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GPT_IMAGE_2, "draw a cube"),
            &ctx(),
            None,
        )
        .await
        .expect_err("decode error");

    mock.assert_async().await;
    assert!(err.to_string().contains("output_format"));
}

#[tokio::test]
async fn image_generation_backend_error_is_opaque() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/images/generations")
        .with_status(500)
        .with_body(format!("backend spilled {API_KEY_SECRET}"))
        .expect(1)
        .create_async()
        .await;
    let provider = provider(&server.url());

    let err = provider
        .generate_image(
            ImageGenerationRequest::new(GPT_IMAGE_2, "draw a cube"),
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

fn assert_image_body(request: &mockito::Request) -> bool {
    let Some(body) = request.utf8_lossy_body().ok() else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&body) else {
        return false;
    };
    value["model"] == GPT_IMAGE_2
        && value["prompt"] == "draw a cube"
        && value["n"] == 2
        && value["size"] == "1024x1024"
        && value["quality"] == "high"
        && value["output_format"] == "png"
        && value["background"] == "transparent"
}

fn assert_hosted_image_body(request: &mockito::Request) -> bool {
    let Some(body) = request.utf8_lossy_body().ok() else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&body) else {
        return false;
    };
    let tools = value["tools"].as_array().cloned().unwrap_or_default();
    let has_image_tool = tools
        .iter()
        .any(|tool| tool["type"] == "image_generation" && tool["output_format"] == "png");
    let input_text = value["input"][0]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    value["model"] == GPT_5_5
        && value["stream"] == true
        && value["tool_choice"] == "auto"
        && value.get("prompt").is_none()
        && value.get("n").is_none()
        && has_image_tool
        && input_text.contains("Generate exactly one image")
        && input_text.contains("draw a cube")
}

fn png_header() -> Vec<u8> {
    b"\x89PNG\r\n\x1a\n".to_vec()
}

fn jpeg_header() -> Vec<u8> {
    b"\xff\xd8\xff\xe0".to_vec()
}
