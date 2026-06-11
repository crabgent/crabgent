//! Response mapping for Google Gemini generateContent calls.

use crabgent_core::{
    Citation, LlmResponse, ModelId, ProviderError, ProviderEvent, StopReason, ToolCall, Usage,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn parse_generate_content_response(
    value: Value,
    fallback_model: ModelId,
) -> Result<(LlmResponse, Option<ProviderEvent>), ProviderError> {
    let raw: RawGenerateContentResponse = serde_json::from_value(value)
        .map_err(|error| ProviderError::MalformedResponse(error.to_string()))?;
    let candidate = raw.candidates.into_iter().next().ok_or_else(|| {
        ProviderError::MalformedResponse("google candidates must not be empty".to_owned())
    })?;
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for part in candidate.content.parts {
        if let Some(chunk) = part.text
            && !part.thought
        {
            text.push_str(&chunk);
        }
        if let Some(function_call) = part.function_call {
            let id = function_call
                .id
                .unwrap_or_else(|| function_call.name.clone());
            tool_calls.push(ToolCall {
                id,
                name: function_call.name,
                args: function_call.args.unwrap_or_else(|| json!({})),
                thought_signature: part.thought_signature,
            });
        }
    }
    let grounding = candidate
        .grounding_metadata
        .as_ref()
        .map(build_grounding_event);
    let response = LlmResponse {
        text,
        tool_calls,
        stop_reason: map_finish_reason(candidate.finish_reason.as_deref()),
        usage: raw.usage_metadata.map_or_else(Usage::default, Into::into),
        model: raw.model_version.unwrap_or(fallback_model),
    };
    Ok((response, grounding))
}

fn build_grounding_event(meta: &RawGroundingMetadata) -> ProviderEvent {
    let citations: Vec<Citation> = meta
        .grounding_chunks
        .iter()
        .filter_map(|chunk| {
            let web = chunk.web.as_ref()?;
            let url = web.uri.clone()?;
            Some(Citation {
                url,
                title: web.title.clone(),
                cited_text: None,
                provider: "google".into(),
                raw: json!(chunk),
            })
        })
        .collect();
    let content = serde_json::to_value(meta).unwrap_or_else(|_| json!({}));
    ProviderEvent::ServerToolResult {
        provider: "google".into(),
        name: "google_search".into(),
        content,
        citations,
    }
}

fn map_finish_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("MAX_TOKENS") => StopReason::MaxTokens,
        Some("STOP") | None => StopReason::EndTurn,
        Some("MALFORMED_FUNCTION_CALL") => StopReason::ToolUse,
        _ => StopReason::Other,
    }
}

#[derive(Deserialize)]
struct RawGenerateContentResponse {
    #[serde(default, rename = "modelVersion")]
    model_version: Option<ModelId>,
    #[serde(default)]
    candidates: Vec<RawCandidate>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<RawUsage>,
}

#[derive(Deserialize)]
struct RawCandidate {
    content: RawContent,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
    #[serde(default, rename = "groundingMetadata")]
    grounding_metadata: Option<RawGroundingMetadata>,
}

#[derive(Deserialize, serde::Serialize)]
struct RawGroundingMetadata {
    #[serde(default, rename = "groundingChunks")]
    grounding_chunks: Vec<RawGroundingChunk>,
    #[serde(default, rename = "groundingSupports")]
    grounding_supports: Vec<Value>,
    #[serde(default, rename = "webSearchQueries")]
    web_search_queries: Vec<String>,
}

#[derive(Deserialize, serde::Serialize)]
struct RawGroundingChunk {
    #[serde(default)]
    web: Option<RawGroundingChunkWeb>,
}

#[derive(Deserialize, serde::Serialize)]
struct RawGroundingChunkWeb {
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    title: Option<String>,
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
    #[serde(default)]
    thought: bool,
    #[serde(default, rename = "functionCall")]
    function_call: Option<RawFunctionCall>,
    #[serde(default, rename = "thoughtSignature", alias = "thought_signature")]
    thought_signature: Option<String>,
}

#[derive(Deserialize)]
struct RawFunctionCall {
    #[serde(default)]
    id: Option<String>,
    name: String,
    #[serde(default)]
    args: Option<Value>,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default, rename = "promptTokenCount")]
    prompt: u32,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates: u32,
    #[serde(default, rename = "cachedContentTokenCount")]
    cached_content: u32,
}

impl From<RawUsage> for Usage {
    fn from(value: RawUsage) -> Self {
        Self {
            input_tokens: value.prompt,
            output_tokens: value.candidates,
            cache_creation_tokens: 0,
            cache_read_tokens: value.cached_content,
        }
    }
}
