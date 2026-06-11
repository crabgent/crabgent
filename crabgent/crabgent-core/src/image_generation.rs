//! Image-generation provider surface and model metadata.

use std::fmt;
use std::num::NonZeroU8;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::hook::RunCtx;
use crate::newtype::string_newtype;

/// Maximum decoded size for generated image payload bytes.
pub const GENERATED_IMAGE_MAX_BYTES: usize = 25_000_000;

const GENERATED_IMAGE_ALLOWED_MIMES: &[&str] =
    &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Abstraction over providers that can generate images.
#[async_trait]
pub trait ImageGenerationProvider: Send + Sync {
    /// Generate one or more images from a prompt.
    async fn generate_image(
        &self,
        req: ImageGenerationRequest,
        ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<ImageGenerationResponse, ImageGenerationError>;

    /// Provider-wide image-generation capability advertisement.
    fn image_generation_capabilities(&self) -> ImageGenerationProviderCapabilities;

    /// Image-generation models this provider can serve.
    fn image_generation_models(&self) -> Vec<ImageGenerationModelInfo> {
        Vec::new()
    }

    /// Fetch the provider's current image-generation model list.
    async fn fetch_image_generation_models(
        &self,
    ) -> Result<Vec<ImageGenerationModelInfo>, ImageGenerationError> {
        Ok(self.image_generation_models())
    }
}

/// Request passed to an image-generation provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageGenerationRequest {
    pub model: ImageGenerationModelId,
    pub prompt: String,
    pub count: NonZeroU8,
    pub size: Option<ImageGenerationSize>,
    pub aspect_ratio: Option<ImageGenerationAspectRatio>,
    pub quality: Option<ImageGenerationQuality>,
    pub format: Option<ImageGenerationFormat>,
    pub background: Option<ImageGenerationBackground>,
}

impl ImageGenerationRequest {
    /// Build a single-image request with provider defaults for output options.
    pub fn new(model: impl Into<ImageGenerationModelId>, prompt: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            prompt: prompt.into(),
            count: NonZeroU8::MIN,
            size: None,
            aspect_ratio: None,
            quality: None,
            format: None,
            background: None,
        }
    }
}

/// Complete image-generation response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageGenerationResponse {
    pub model: ImageGenerationModelId,
    pub images: Vec<GeneratedImage>,
    pub text: Option<String>,
    pub usage: Option<ImageGenerationUsage>,
}

/// Decoded generated-image bytes plus MIME type.
#[derive(Clone, PartialEq, Eq)]
pub struct GeneratedImage {
    bytes: Arc<[u8]>,
    mime: String,
    pub revised_prompt: Option<String>,
}

impl GeneratedImage {
    pub fn new(
        bytes: impl Into<Arc<[u8]>>,
        mime: impl Into<String>,
        revised_prompt: Option<String>,
    ) -> Result<Self, ImageGenerationError> {
        let bytes = bytes.into();
        let mime = mime.into();
        validate_generated_image(bytes.as_ref(), &mime)?;
        Ok(Self {
            bytes,
            mime,
            revised_prompt,
        })
    }

    #[must_use]
    pub const fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    #[must_use]
    pub fn mime(&self) -> &str {
        &self.mime
    }
}

impl fmt::Debug for GeneratedImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeneratedImage")
            .field("bytes", &ByteLen(self.bytes.len()))
            .field("mime", &self.mime)
            .field("revised_prompt", &self.revised_prompt)
            .finish()
    }
}

#[derive(Clone, Copy)]
struct ByteLen(usize);

impl fmt::Debug for ByteLen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} bytes", self.0)
    }
}

fn validate_generated_image(bytes: &[u8], mime: &str) -> Result<(), ImageGenerationError> {
    let byte_len = bytes.len();
    if byte_len > GENERATED_IMAGE_MAX_BYTES {
        return Err(ImageGenerationError::Decode(format!(
            "generated image exceeds {GENERATED_IMAGE_MAX_BYTES} bytes"
        )));
    }
    if !GENERATED_IMAGE_ALLOWED_MIMES.contains(&mime) {
        return Err(ImageGenerationError::Decode(format!(
            "unsupported generated image MIME type: {mime}"
        )));
    }
    if !mime_matches_bytes(mime, bytes) {
        return Err(ImageGenerationError::Decode(format!(
            "generated image bytes do not match MIME type: {mime}"
        )));
    }
    Ok(())
}

fn mime_matches_bytes(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => {
            bytes.starts_with(b"RIFF") && bytes.get(8..12).is_some_and(|chunk| chunk == b"WEBP")
        }
        _ => false,
    }
}

/// Stable identifier of an image-generation model.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImageGenerationModelId(String);

string_newtype!(trim ImageGenerationModelId);

/// Provider-neutral output size hint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImageGenerationSize(String);

string_newtype!(passthrough ImageGenerationSize);

/// Provider-neutral aspect-ratio hint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImageGenerationAspectRatio(String);

string_newtype!(passthrough ImageGenerationAspectRatio);

/// Provider-neutral quality hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImageGenerationQuality {
    Auto,
    Low,
    Medium,
    High,
}

impl ImageGenerationQuality {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Provider-neutral output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImageGenerationFormat {
    Png,
    Jpeg,
    Webp,
}

impl ImageGenerationFormat {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
        }
    }

    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }
}

/// Provider-neutral background hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImageGenerationBackground {
    Auto,
    Opaque,
    Transparent,
}

impl ImageGenerationBackground {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Opaque => "opaque",
            Self::Transparent => "transparent",
        }
    }
}

/// Metadata for one image-generation model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageGenerationModelInfo {
    pub id: ImageGenerationModelId,
    pub display_name: String,
    pub supports_editing: bool,
    pub supports_transparent_background: bool,
}

/// Provider-wide image-generation capabilities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageGenerationProviderCapabilities {
    pub generation: bool,
    pub editing: bool,
}

/// Token usage stats for one image-generation call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImageGenerationUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

/// Errors returned by image-generation providers.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImageGenerationError {
    #[error("auth error: {0}")]
    Auth(String),
    #[error("network error")]
    Network,
    #[error("backend error: {0}")]
    Backend(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("image generation not supported by provider '{provider}' model '{model}'")]
    Unsupported { provider: String, model: String },
    #[error(
        "image-generation option '{option}' not supported by provider '{provider}' model '{model}'"
    )]
    UnsupportedOption {
        provider: String,
        model: String,
        option: String,
    },
    #[error("invalid image-generation request: {0}")]
    InvalidRequest(String),
    #[error("model discovery failed: {reason}")]
    ModelDiscovery { reason: String },
    #[error("unknown image-generation model")]
    ModelUnknown,
    #[error("cancelled")]
    Cancelled,
    #[error("timeout")]
    Timeout,
}
