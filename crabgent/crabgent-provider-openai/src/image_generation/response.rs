//! Wire-format parsing for image generation responses: the Images API JSON
//! shape and the Codex hosted-tool SSE/JSON stream, plus base64 image decode.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use crabgent_core::{
    GeneratedImage, ImageGenerationError, ImageGenerationFormat, ImageGenerationRequest,
    ImageGenerationResponse, ImageGenerationUsage,
};
use crabgent_log::warn;
use serde::Deserialize;
use serde_json::Value;

use super::{HOSTED_OUTPUT_FORMAT, MAX_WIRE_IMAGE_BYTES, decode_error};

pub(super) fn parse_images_response(
    raw: RawImagesResponse,
    req: &ImageGenerationRequest,
) -> Result<ImageGenerationResponse, ImageGenerationError> {
    let mime = response_mime(raw.output_format.as_deref(), req.format)?;
    let images = raw
        .data
        .into_iter()
        .map(|image| parse_generated_image(image, mime))
        .collect::<Result<Vec<_>, _>>()?;
    if images.is_empty() {
        return Err(decode_error("image response did not include any images"));
    }
    Ok(ImageGenerationResponse {
        model: req.model.clone(),
        images,
        text: None,
        usage: raw.usage.map(Into::into),
    })
}

pub(super) fn parse_hosted_image_response(
    bytes: &[u8],
    req: &ImageGenerationRequest,
) -> Result<ImageGenerationResponse, ImageGenerationError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| decode_error(format!("invalid utf-8: {error}")))?;
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return parse_hosted_image_json(&value, req);
    }
    parse_hosted_image_sse(text, req)
}

fn parse_hosted_image_sse(
    text: &str,
    req: &ImageGenerationRequest,
) -> Result<ImageGenerationResponse, ImageGenerationError> {
    let mut pending = String::new();
    let mut calls = Vec::new();
    let mut usage = None;
    for line in text.lines() {
        let Some(data) = hosted_sse_data(line, !pending.is_empty()) else {
            continue;
        };
        if data == "[DONE]" {
            continue;
        }
        pending.push_str(data);
        let Ok(value) = serde_json::from_str::<Value>(&pending) else {
            continue;
        };
        pending.clear();
        collect_hosted_image_event(&value, &mut calls, &mut usage);
    }
    if !pending.is_empty() {
        return Err(decode_error("hosted image SSE ended with incomplete JSON"));
    }
    build_hosted_image_response(calls, usage, req)
}

fn hosted_sse_data(line: &str, has_pending: bool) -> Option<&str> {
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(data) = trimmed.strip_prefix("data:") {
        return Some(data.strip_prefix(' ').unwrap_or(data));
    }
    has_pending.then_some(trimmed)
}

fn parse_hosted_image_json(
    value: &Value,
    req: &ImageGenerationRequest,
) -> Result<ImageGenerationResponse, ImageGenerationError> {
    let mut calls = Vec::new();
    let mut usage = None;
    collect_hosted_image_response(value, &mut calls, &mut usage);
    build_hosted_image_response(calls, usage, req)
}

fn collect_hosted_image_event(
    value: &Value,
    calls: &mut Vec<RawHostedImageCall>,
    usage: &mut Option<RawUsage>,
) {
    if value.get("type").and_then(Value::as_str) == Some("response.output_item.done") {
        let item = value.get("item").unwrap_or(value);
        collect_hosted_image_call(item, calls);
    }
    if let Some(response) = value.get("response") {
        collect_hosted_image_response(response, calls, usage);
    } else {
        collect_hosted_image_response(value, calls, usage);
    }
}

fn collect_hosted_image_response(
    value: &Value,
    calls: &mut Vec<RawHostedImageCall>,
    usage: &mut Option<RawUsage>,
) {
    if let Some(raw_usage) = value.get("usage")
        && let Ok(parsed) = serde_json::from_value::<RawUsage>(raw_usage.clone())
    {
        *usage = Some(parsed);
    }
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for item in output {
            collect_hosted_image_call(item, calls);
        }
    } else {
        collect_hosted_image_call(value, calls);
    }
}

fn collect_hosted_image_call(value: &Value, calls: &mut Vec<RawHostedImageCall>) {
    if value.get("type").and_then(Value::as_str) != Some("image_generation_call") {
        return;
    }
    match serde_json::from_value::<RawHostedImageCall>(value.clone()) {
        Ok(call) => calls.push(call),
        Err(error) => warn!(
            error = %error,
            "openai hosted image generation response included malformed image call"
        ),
    }
}

fn build_hosted_image_response(
    calls: Vec<RawHostedImageCall>,
    usage: Option<RawUsage>,
    req: &ImageGenerationRequest,
) -> Result<ImageGenerationResponse, ImageGenerationError> {
    // The Codex backend used to mark the final `output_item.done` call as
    // "completed" but now leaves it at "generating" even though `result`
    // already carries the finished image, so both statuses count as done.
    // Partial frames never reach this filter: they arrive as
    // `response.image_generation_call.partial_image` events, which the
    // collectors above do not treat as image calls.
    let images = calls
        .into_iter()
        .filter(|call| {
            call.status
                .as_deref()
                .is_none_or(|status| status == "completed" || status == "generating")
        })
        .map(parse_hosted_image_call)
        .collect::<Result<Vec<_>, _>>()?;
    if images.is_empty() {
        return Err(decode_error(
            "hosted image response did not include a completed image_generation_call",
        ));
    }
    Ok(ImageGenerationResponse {
        model: req.model.clone(),
        images,
        text: None,
        usage: usage.map(Into::into),
    })
}

fn parse_hosted_image_call(
    raw: RawHostedImageCall,
) -> Result<GeneratedImage, ImageGenerationError> {
    parse_generated_image_data(raw.result, HOSTED_OUTPUT_FORMAT.mime(), raw.revised_prompt)
}

fn parse_generated_image(
    raw: RawImage,
    mime: &'static str,
) -> Result<GeneratedImage, ImageGenerationError> {
    let data = raw
        .b64_json
        .ok_or_else(|| decode_error("image response did not include b64_json"))?;
    parse_generated_image_data(data, mime, raw.revised_prompt)
}

fn parse_generated_image_data(
    data: String,
    mime: &'static str,
    revised_prompt: Option<String>,
) -> Result<GeneratedImage, ImageGenerationError> {
    if data.len() > MAX_WIRE_IMAGE_BYTES {
        return Err(decode_error("image response exceeds max wire size"));
    }
    // Wire-format decode of a generation API response: the model produced the
    // image and the API returns it base64-encoded. This is the provider's own
    // wire responsibility (inverse of the vision-inbound path that core owns),
    // so it is not the "base64 outside core transport serde" anti-pattern.
    let bytes = BASE64_STANDARD
        .decode(data)
        .map_err(|error| decode_error(error.to_string()))?;
    GeneratedImage::new(bytes.into_boxed_slice(), mime, revised_prompt)
}

fn response_mime(
    response_format: Option<&str>,
    request_format: Option<ImageGenerationFormat>,
) -> Result<&'static str, ImageGenerationError> {
    match response_format {
        Some("png") => Ok("image/png"),
        Some("jpeg") => Ok("image/jpeg"),
        Some("webp") => Ok("image/webp"),
        Some(other) => Err(decode_error(format!("unsupported output_format: {other}"))),
        None => Ok(request_format.unwrap_or(ImageGenerationFormat::Png).mime()),
    }
}

#[derive(Deserialize)]
pub(super) struct RawImagesResponse {
    #[serde(default)]
    data: Vec<RawImage>,
    #[serde(default)]
    output_format: Option<String>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct RawImage {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    revised_prompt: Option<String>,
}

#[derive(Deserialize)]
struct RawHostedImageCall {
    result: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    revised_prompt: Option<String>,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default, rename = "input_tokens")]
    input: u32,
    #[serde(default, rename = "output_tokens")]
    output: u32,
    #[serde(default, rename = "total_tokens")]
    total: u32,
}

impl From<RawUsage> for ImageGenerationUsage {
    fn from(value: RawUsage) -> Self {
        Self {
            input_tokens: value.input,
            output_tokens: value.output,
            total_tokens: value.total,
        }
    }
}
