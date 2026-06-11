//! Image-generation support for Gemini image models.

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use crabgent_core::{
    GENERATED_IMAGE_MAX_BYTES, GeneratedImage, ImageGenerationError, ImageGenerationModelInfo,
    ImageGenerationProvider, ImageGenerationProviderCapabilities, ImageGenerationRequest,
    ImageGenerationResponse, ImageGenerationUsage, RunCtx,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::client;
use crate::models::{self, PROVIDER};
use crate::types::{GoogleConfig, GoogleError};

const MAX_WIRE_IMAGE_BYTES: usize = GENERATED_IMAGE_MAX_BYTES * 4 / 3 + 16;
const RESPONSE_JSON_OVERHEAD_BYTES: usize = 1024 * 1024;
const MAX_IMAGES_PER_REQUEST: u8 = 4;

/// Google Gemini image-generation provider.
pub struct GoogleImageGenerationProvider {
    http: reqwest::Client,
    config: GoogleConfig,
}

impl GoogleImageGenerationProvider {
    pub fn try_new(http: reqwest::Client, config: GoogleConfig) -> Result<Self, GoogleError> {
        crate::validate_config(&config)?;
        Ok(Self { http, config })
    }

    #[must_use]
    pub const fn config(&self) -> &GoogleConfig {
        &self.config
    }
}

#[async_trait]
impl ImageGenerationProvider for GoogleImageGenerationProvider {
    async fn generate_image(
        &self,
        req: ImageGenerationRequest,
        _ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<ImageGenerationResponse, ImageGenerationError> {
        validate_request(&req)?;
        let body = image_generation_body(&req);
        let value = client::post_json(
            &self.http,
            &self.config,
            req.model.as_str(),
            &body,
            max_response_bytes(req.count.get()),
            cancel,
        )
        .await?;
        parse_image_response(value, req)
    }

    fn image_generation_capabilities(&self) -> ImageGenerationProviderCapabilities {
        ImageGenerationProviderCapabilities {
            generation: true,
            editing: false,
        }
    }

    fn image_generation_models(&self) -> Vec<ImageGenerationModelInfo> {
        models::google_image_generation_models()
    }
}

fn image_generation_body(req: &ImageGenerationRequest) -> Value {
    let mut generation_config = Map::new();
    generation_config.insert("responseModalities".to_owned(), json!(["TEXT", "IMAGE"]));
    generation_config.insert("candidateCount".to_owned(), json!(req.count.get()));
    if let Some(aspect_ratio) = &req.aspect_ratio {
        generation_config.insert(
            "imageConfig".to_owned(),
            json!({"aspectRatio": aspect_ratio.as_str()}),
        );
    }
    json!({
        "contents": [{
            "role": "user",
            "parts": [{"text": req.prompt}],
        }],
        "generationConfig": generation_config,
    })
}

fn validate_request(req: &ImageGenerationRequest) -> Result<(), ImageGenerationError> {
    if req.count.get() > MAX_IMAGES_PER_REQUEST {
        return Err(ImageGenerationError::InvalidRequest(format!(
            "google image generation supports at most {MAX_IMAGES_PER_REQUEST} images per request"
        )));
    }
    if req.size.is_some() {
        return Err(unsupported_option(req, "size"));
    }
    if req.quality.is_some() {
        return Err(unsupported_option(req, "quality"));
    }
    if req.format.is_some() {
        return Err(unsupported_option(req, "format"));
    }
    if req.background.is_some() {
        return Err(unsupported_option(req, "background"));
    }
    Ok(())
}

fn unsupported_option(req: &ImageGenerationRequest, option: &str) -> ImageGenerationError {
    ImageGenerationError::UnsupportedOption {
        provider: PROVIDER.to_owned(),
        model: req.model.to_string(),
        option: option.to_owned(),
    }
}

fn max_response_bytes(count: u8) -> usize {
    MAX_WIRE_IMAGE_BYTES
        .saturating_mul(usize::from(count))
        .saturating_add(RESPONSE_JSON_OVERHEAD_BYTES)
}

fn parse_image_response(
    value: Value,
    req: ImageGenerationRequest,
) -> Result<ImageGenerationResponse, ImageGenerationError> {
    let raw: RawGenerateContentResponse =
        serde_json::from_value(value).map_err(|error| decode_error(error.to_string()))?;
    let mut images = Vec::new();
    let mut text = String::new();
    let usage = raw.usage_metadata.map(Into::into);
    for candidate in raw.candidates {
        for part in candidate.content.parts {
            if let Some(chunk) = part.text {
                text.push_str(&chunk);
            }
            if let Some(inline_data) = part.inline_data {
                images.push(parse_inline_image(inline_data)?);
            }
        }
    }
    if images.is_empty() {
        return Err(decode_error(
            "google image response did not include inlineData",
        ));
    }
    Ok(ImageGenerationResponse {
        model: req.model,
        images,
        text: (!text.is_empty()).then_some(text),
        usage,
    })
}

fn parse_inline_image(raw: RawInlineData) -> Result<GeneratedImage, ImageGenerationError> {
    if raw.data.len() > MAX_WIRE_IMAGE_BYTES {
        return Err(decode_error("image response exceeds max wire size"));
    }
    // Wire-format decode of a generation API response: the model produced the
    // image and the API returns it base64-encoded. This is the provider's own
    // wire responsibility (inverse of the vision-inbound path that core owns),
    // so it is not the "base64 outside core transport serde" anti-pattern.
    let bytes = BASE64_STANDARD
        .decode(raw.data)
        .map_err(|error| decode_error(error.to_string()))?;
    GeneratedImage::new(bytes.into_boxed_slice(), raw.mime_type, None)
}

fn decode_error(message: impl Into<String>) -> ImageGenerationError {
    ImageGenerationError::Decode(message.into())
}

impl From<GoogleError> for ImageGenerationError {
    fn from(error: GoogleError) -> Self {
        match error {
            GoogleError::Auth => Self::Auth("google authentication failed".to_owned()),
            GoogleError::Network => Self::Network,
            GoogleError::Api { .. } => {
                Self::Backend("google image generation request failed".to_owned())
            }
            GoogleError::MalformedResponse(message) => Self::Decode(message),
            GoogleError::ConfigError(message) => Self::Backend(message),
            GoogleError::Cancelled => Self::Cancelled,
            GoogleError::Timeout => Self::Timeout,
        }
    }
}

#[derive(Deserialize)]
struct RawGenerateContentResponse {
    #[serde(default)]
    candidates: Vec<RawCandidate>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<RawUsage>,
}

#[derive(Deserialize)]
struct RawCandidate {
    content: RawContent,
}

#[derive(Deserialize)]
struct RawContent {
    #[serde(default)]
    parts: Vec<RawPart>,
}

#[derive(Deserialize)]
struct RawPart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default, rename = "inlineData")]
    inline_data: Option<RawInlineData>,
}

#[derive(Deserialize)]
struct RawInlineData {
    #[serde(rename = "mimeType")]
    mime_type: String,
    data: String,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default, rename = "promptTokenCount")]
    prompt: u32,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates: u32,
    #[serde(default, rename = "totalTokenCount")]
    total: u32,
}

impl From<RawUsage> for ImageGenerationUsage {
    fn from(value: RawUsage) -> Self {
        Self {
            input_tokens: value.prompt,
            output_tokens: value.candidates,
            total_tokens: value.total,
        }
    }
}
