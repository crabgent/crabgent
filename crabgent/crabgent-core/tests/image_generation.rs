use async_trait::async_trait;
use crabgent_core::{
    GENERATED_IMAGE_MAX_BYTES, GeneratedImage, ImageGenerationAspectRatio,
    ImageGenerationBackground, ImageGenerationError, ImageGenerationFormat, ImageGenerationModelId,
    ImageGenerationModelInfo, ImageGenerationProvider, ImageGenerationProviderCapabilities,
    ImageGenerationQuality, ImageGenerationRequest, ImageGenerationResponse, ImageGenerationSize,
    RunCtx, RunId, Subject,
};
use tokio_util::sync::CancellationToken;

struct StubImageGenerationProvider;

#[async_trait]
impl ImageGenerationProvider for StubImageGenerationProvider {
    async fn generate_image(
        &self,
        req: ImageGenerationRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ImageGenerationResponse, ImageGenerationError> {
        Ok(ImageGenerationResponse {
            model: req.model,
            images: Vec::new(),
            text: Some("stub".to_owned()),
            usage: None,
        })
    }

    fn image_generation_capabilities(&self) -> ImageGenerationProviderCapabilities {
        ImageGenerationProviderCapabilities {
            generation: true,
            editing: false,
        }
    }

    fn image_generation_models(&self) -> Vec<ImageGenerationModelInfo> {
        vec![ImageGenerationModelInfo {
            id: ImageGenerationModelId::new("stub-image"),
            display_name: "Stub Image".to_owned(),
            supports_editing: false,
            supports_transparent_background: false,
        }]
    }
}

#[test]
fn image_generation_model_id_trims_whitespace() {
    let id = ImageGenerationModelId::new(" gpt-image-1.5 ");
    assert_eq!(id.as_str(), "gpt-image-1.5");
}

#[test]
fn request_defaults_to_one_image() {
    let req = ImageGenerationRequest::new("gpt-image-1.5", "draw a cube");
    assert_eq!(req.count.get(), 1);
    assert_eq!(req.model.as_str(), "gpt-image-1.5");
    assert_eq!(req.prompt, "draw a cube");
    assert!(req.size.is_none());
    assert!(req.format.is_none());
}

#[test]
fn generated_image_debug_masks_bytes() {
    let image = GeneratedImage::new(png_header().into_boxed_slice(), "image/png", None)
        .expect("valid generated image");

    let rendered = format!("{image:?}");

    assert!(rendered.contains("8 bytes"));
    assert!(!rendered.contains("137"));
}

#[test]
fn generated_image_rejects_unsupported_mime() {
    let err = GeneratedImage::new(
        png_header().into_boxed_slice(),
        "application/octet-stream",
        None,
    )
    .expect_err("unsupported MIME rejected");

    assert!(matches!(err, ImageGenerationError::Decode(_)));
}

#[test]
fn generated_image_rejects_mime_mismatch() {
    let err = GeneratedImage::new(png_header().into_boxed_slice(), "image/jpeg", None)
        .expect_err("MIME mismatch rejected");

    assert!(err.to_string().contains("MIME type"));
}

#[test]
fn output_format_maps_to_mime() {
    assert_eq!(ImageGenerationFormat::Png.as_str(), "png");
    assert_eq!(ImageGenerationFormat::Jpeg.mime(), "image/jpeg");
    assert_eq!(ImageGenerationFormat::Webp.as_str(), "webp");
    assert_eq!(ImageGenerationFormat::Webp.mime(), "image/webp");
}

#[test]
fn generated_image_accepts_supported_magic_bytes() {
    let jpeg = GeneratedImage::new(jpeg_header().into_boxed_slice(), "image/jpeg", None)
        .expect("jpeg accepted");
    let gif = GeneratedImage::new(gif_header().into_boxed_slice(), "image/gif", None)
        .expect("gif accepted");
    let webp = GeneratedImage::new(webp_header().into_boxed_slice(), "image/webp", None)
        .expect("webp accepted");

    assert_eq!(jpeg.mime(), "image/jpeg");
    assert_eq!(gif.mime(), "image/gif");
    assert_eq!(webp.mime(), "image/webp");
    assert_eq!(webp.bytes().as_ref(), webp_header().as_slice());
}

#[test]
fn generated_image_rejects_too_large() {
    let bytes = vec![0; GENERATED_IMAGE_MAX_BYTES + 1];

    let err = GeneratedImage::new(bytes.into_boxed_slice(), "image/png", None)
        .expect_err("oversized generated image rejected");

    assert!(err.to_string().contains("exceeds"));
}

#[test]
fn request_option_helpers_preserve_provider_neutral_strings() {
    let size = ImageGenerationSize::new("1536x1024");
    let aspect_ratio = ImageGenerationAspectRatio::new("16:9");

    assert_eq!(size.as_str(), "1536x1024");
    assert_eq!(aspect_ratio.as_str(), "16:9");
    assert_eq!(ImageGenerationQuality::Auto.as_str(), "auto");
    assert_eq!(ImageGenerationQuality::Low.as_str(), "low");
    assert_eq!(ImageGenerationQuality::Medium.as_str(), "medium");
    assert_eq!(ImageGenerationQuality::High.as_str(), "high");
    assert_eq!(ImageGenerationBackground::Auto.as_str(), "auto");
    assert_eq!(ImageGenerationBackground::Opaque.as_str(), "opaque");
    assert_eq!(
        ImageGenerationBackground::Transparent.as_str(),
        "transparent"
    );
}

#[test]
fn model_id_from_string_variants_trim() {
    let owned = ImageGenerationModelId::from(" model-a ".to_owned());
    let borrowed = ImageGenerationModelId::from(&" model-b ".to_owned());

    assert_eq!(owned.as_str(), "model-a");
    assert_eq!(borrowed.to_string(), "model-b");
}

#[tokio::test]
async fn image_generation_provider_is_object_safe() {
    let provider: &dyn ImageGenerationProvider = &StubImageGenerationProvider;
    let ctx = RunCtx::new(RunId::new(), Subject::new("test-subject"));

    let response = provider
        .generate_image(
            ImageGenerationRequest::new("stub-image", "draw a cube"),
            &ctx,
            None,
        )
        .await
        .expect("stub provider succeeds");

    assert_eq!(response.model.as_str(), "stub-image");
    assert_eq!(response.text.as_deref(), Some("stub"));
}

#[tokio::test]
async fn fetch_image_generation_models_default_returns_models() {
    let provider = StubImageGenerationProvider;

    let models = provider
        .fetch_image_generation_models()
        .await
        .expect("models");

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id.as_str(), "stub-image");
}

fn png_header() -> Vec<u8> {
    b"\x89PNG\r\n\x1a\n".to_vec()
}

fn jpeg_header() -> Vec<u8> {
    b"\xff\xd8\xff\xe0".to_vec()
}

fn gif_header() -> Vec<u8> {
    b"GIF89a".to_vec()
}

fn webp_header() -> Vec<u8> {
    b"RIFF\x00\x00\x00\x00WEBP".to_vec()
}
