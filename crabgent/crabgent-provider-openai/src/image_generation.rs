//! Image-generation support for `OpenAI` Image API and Codex hosted image tool.

mod http;
mod response;

use async_trait::async_trait;
use crabgent_core::{
    GENERATED_IMAGE_MAX_BYTES, ImageGenerationError, ImageGenerationFormat, ImageGenerationModelId,
    ImageGenerationModelInfo, ImageGenerationProvider, ImageGenerationProviderCapabilities,
    ImageGenerationRequest, ImageGenerationResponse, RunCtx,
};
use secrecy::ExposeSecret;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::auth::{ApiKeyAuth, AuthStrategy};
use crate::models::GPT_5_5;
use crate::types::{OpenAiConfig, OpenAiError};
use crate::wire::responses::prompt_cache_key;

use http::{ImageRequestCtx, read_body, send_with_retry};
use response::{RawImagesResponse, parse_hosted_image_response, parse_images_response};

const ENDPOINT_PATH: &str = "/v1/images/generations";
const HOSTED_TOOL_NAME: &str = "image_generation";
const HOSTED_OUTPUT_FORMAT: ImageGenerationFormat = ImageGenerationFormat::Png;
const MAX_WIRE_IMAGE_BYTES: usize = GENERATED_IMAGE_MAX_BYTES * 4 / 3 + 16;
const RESPONSE_JSON_OVERHEAD_BYTES: usize = 1024 * 1024;
const MAX_IMAGES_PER_REQUEST: u8 = 10;

pub const GPT_IMAGE_2: &str = "gpt-image-2";
pub const GPT_IMAGE_1_5: &str = "gpt-image-1.5";
pub const GPT_IMAGE_1: &str = "gpt-image-1";
pub const GPT_IMAGE_1_MINI: &str = "gpt-image-1-mini";

/// `OpenAI` Image API provider.
pub struct OpenAiImageGenerationProvider {
    http: reqwest::Client,
    config: OpenAiConfig,
    auth: Box<dyn AuthStrategy>,
}

impl OpenAiImageGenerationProvider {
    pub fn try_new(
        http: reqwest::Client,
        config: OpenAiConfig,
        auth: Box<dyn AuthStrategy>,
    ) -> Result<Self, OpenAiError> {
        validate_config(&config)?;
        Ok(Self { http, config, auth })
    }

    pub fn try_from_api_key(
        http: reqwest::Client,
        config: OpenAiConfig,
    ) -> Result<Self, OpenAiError> {
        let auth = ApiKeyAuth::new(config.api_key.clone());
        Self::try_new(http, config, Box::new(auth))
    }

    #[must_use]
    pub const fn config(&self) -> &OpenAiConfig {
        &self.config
    }

    #[must_use]
    pub fn auth(&self) -> &dyn AuthStrategy {
        self.auth.as_ref()
    }
}

#[async_trait]
impl ImageGenerationProvider for OpenAiImageGenerationProvider {
    async fn generate_image(
        &self,
        req: ImageGenerationRequest,
        ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<ImageGenerationResponse, ImageGenerationError> {
        if self.auth.supports_hosted_image_generation() {
            return self.generate_hosted_image(req, ctx, cancel).await;
        }
        validate_images_api_request(&req)?;
        let body = images_api_body(&req);
        let response = send_with_retry(
            &ImageRequestCtx {
                http: &self.http,
                config: &self.config,
                auth: self.auth.as_ref(),
                ctx,
                body: &body,
                endpoint_path: ENDPOINT_PATH,
            },
            cancel,
        )
        .await?;
        let bytes = read_body(
            response,
            cancel,
            self.config.request_timeout,
            max_response_bytes(req.count.get()),
        )
        .await?;
        let raw: RawImagesResponse =
            serde_json::from_slice(&bytes).map_err(|error| decode_error(error.to_string()))?;
        parse_images_response(raw, &req)
    }

    fn image_generation_capabilities(&self) -> ImageGenerationProviderCapabilities {
        ImageGenerationProviderCapabilities {
            generation: true,
            editing: false,
        }
    }

    fn image_generation_models(&self) -> Vec<ImageGenerationModelInfo> {
        openai_image_generation_models()
    }
}

impl OpenAiImageGenerationProvider {
    async fn generate_hosted_image(
        &self,
        req: ImageGenerationRequest,
        ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<ImageGenerationResponse, ImageGenerationError> {
        validate_hosted_image_request(&req)?;
        let body = hosted_image_body(&req);
        let response = send_with_retry(
            &ImageRequestCtx {
                http: &self.http,
                config: &self.config,
                auth: self.auth.as_ref(),
                ctx,
                body: &body,
                endpoint_path: self.auth.wire().endpoint_path(),
            },
            cancel,
        )
        .await?;
        let bytes = read_body(
            response,
            cancel,
            self.config.request_timeout,
            max_hosted_response_bytes(),
        )
        .await?;
        parse_hosted_image_response(&bytes, &req)
    }
}

#[must_use]
pub fn openai_image_generation_models() -> Vec<ImageGenerationModelInfo> {
    vec![
        image_model(GPT_IMAGE_2, "GPT Image 2"),
        image_model(GPT_IMAGE_1_5, "GPT Image 1.5"),
        image_model(GPT_IMAGE_1, "GPT Image 1"),
        image_model(GPT_IMAGE_1_MINI, "GPT Image 1 Mini"),
    ]
}

fn image_model(id: &'static str, display_name: &'static str) -> ImageGenerationModelInfo {
    ImageGenerationModelInfo {
        id: ImageGenerationModelId::new(id),
        display_name: display_name.to_owned(),
        supports_editing: false,
        supports_transparent_background: true,
    }
}

fn validate_images_api_request(req: &ImageGenerationRequest) -> Result<(), ImageGenerationError> {
    if req.count.get() > MAX_IMAGES_PER_REQUEST {
        return Err(ImageGenerationError::InvalidRequest(format!(
            "openai image generation supports at most {MAX_IMAGES_PER_REQUEST} images per request"
        )));
    }
    if req.aspect_ratio.is_some() {
        return Err(ImageGenerationError::UnsupportedOption {
            provider: "openai".to_owned(),
            model: req.model.to_string(),
            option: "aspect_ratio".to_owned(),
        });
    }
    Ok(())
}

fn validate_hosted_image_request(req: &ImageGenerationRequest) -> Result<(), ImageGenerationError> {
    if req.count.get() != 1 {
        return Err(ImageGenerationError::UnsupportedOption {
            provider: "openai".to_owned(),
            model: req.model.to_string(),
            option: "count".to_owned(),
        });
    }
    if req.aspect_ratio.is_some() {
        return Err(ImageGenerationError::UnsupportedOption {
            provider: "openai".to_owned(),
            model: req.model.to_string(),
            option: "aspect_ratio".to_owned(),
        });
    }
    Ok(())
}

fn max_response_bytes(count: u8) -> usize {
    MAX_WIRE_IMAGE_BYTES
        .saturating_mul(usize::from(count))
        .saturating_add(RESPONSE_JSON_OVERHEAD_BYTES)
}

const fn max_hosted_response_bytes() -> usize {
    MAX_WIRE_IMAGE_BYTES
        .saturating_mul(2)
        .saturating_add(RESPONSE_JSON_OVERHEAD_BYTES)
}

fn images_api_body(req: &ImageGenerationRequest) -> Value {
    let mut body = Map::new();
    body.insert("model".to_owned(), json!(req.model));
    body.insert("prompt".to_owned(), Value::String(req.prompt.clone()));
    body.insert("n".to_owned(), json!(req.count.get()));
    if let Some(size) = &req.size {
        body.insert("size".to_owned(), Value::String(size.as_str().to_owned()));
    }
    if let Some(quality) = req.quality {
        body.insert(
            "quality".to_owned(),
            Value::String(quality.as_str().to_owned()),
        );
    }
    if let Some(format) = req.format {
        body.insert(
            "output_format".to_owned(),
            Value::String(format.as_str().to_owned()),
        );
    }
    if let Some(background) = req.background {
        body.insert(
            "background".to_owned(),
            Value::String(background.as_str().to_owned()),
        );
    }
    Value::Object(body)
}

fn hosted_image_body(req: &ImageGenerationRequest) -> Value {
    let mut body = Map::new();
    body.insert("model".to_owned(), Value::String(GPT_5_5.to_owned()));
    body.insert("store".to_owned(), Value::Bool(false));
    body.insert("stream".to_owned(), Value::Bool(true));
    body.insert(
        "prompt_cache_key".to_owned(),
        Value::String(prompt_cache_key("", &[HOSTED_TOOL_NAME])),
    );
    body.insert(
        "client_metadata".to_owned(),
        json!({"x-codex-installation-id": crate::auth::CODEX_INSTALLATION_ID}),
    );
    body.insert(
        "tools".to_owned(),
        json!([{"type": HOSTED_TOOL_NAME, "output_format": HOSTED_OUTPUT_FORMAT.as_str()}]),
    );
    body.insert("tool_choice".to_owned(), Value::String("auto".to_owned()));
    body.insert("parallel_tool_calls".to_owned(), Value::Bool(false));
    body.insert("input".to_owned(), json!([hosted_image_user_message(req)]));
    Value::Object(body)
}

fn hosted_image_user_message(req: &ImageGenerationRequest) -> Value {
    json!({
        "role": "user",
        "content": [{
            "type": "input_text",
            "text": hosted_image_prompt(req),
        }],
    })
}

fn hosted_image_prompt(req: &ImageGenerationRequest) -> String {
    let mut text = String::from(
        "Generate exactly one image with the image_generation tool. \
         Do not answer with prose unless the image tool fails.\n\nImage prompt:\n",
    );
    text.push_str(&req.prompt);
    if let Some(size) = &req.size {
        text.push_str("\n\nRequested size: ");
        text.push_str(size.as_str());
    }
    if let Some(quality) = req.quality {
        text.push_str("\nRequested quality: ");
        text.push_str(quality.as_str());
    }
    if let Some(background) = req.background {
        text.push_str("\nRequested background: ");
        text.push_str(background.as_str());
    }
    text
}

fn decode_error(message: impl Into<String>) -> ImageGenerationError {
    ImageGenerationError::Decode(message.into())
}

fn validate_config(config: &OpenAiConfig) -> Result<(), OpenAiError> {
    if config.api_key.expose_secret().trim().is_empty() {
        return Err(OpenAiError::ConfigError(
            "openai api_key must not be empty".to_owned(),
        ));
    }
    if config.request_timeout.is_zero() {
        return Err(OpenAiError::ConfigError(
            "openai request_timeout must be > 0".to_owned(),
        ));
    }
    Ok(())
}
