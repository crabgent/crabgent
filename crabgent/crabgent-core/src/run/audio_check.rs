//! Audio capability pre-flight before provider calls.

use serde_json::Value;

use crate::error::ProviderError;
use crate::model::ModelCapabilities;
use crate::provider::ProviderCapabilities;
use crate::types::LlmRequest;

pub(super) fn check_audio_capability(
    req: &LlmRequest,
    provider_caps: &ProviderCapabilities,
    model_caps: &ModelCapabilities,
    provider_name: &str,
    model_id: &str,
) -> Result<(), ProviderError> {
    if !contains_audio(&req.messages) {
        return Ok(());
    }

    if provider_caps.audio && model_caps.supports_audio {
        return Ok(());
    }

    Err(ProviderError::AudioUnsupported {
        provider: provider_name.to_owned(),
        model: model_id.to_owned(),
    })
}

fn contains_audio(messages: &[Value]) -> bool {
    messages.iter().any(message_contains_audio)
}

fn message_contains_audio(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|content| content.iter().any(is_audio_block))
}

fn is_audio_block(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("audio")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn request(messages: Vec<Value>) -> LlmRequest {
        LlmRequest {
            model: "m".into(),
            system_prompt: None,
            messages,
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            stop_sequences: Vec::new(),
            reasoning_effort: None,
            web_search: crate::types::WebSearchConfig::default(),
            tool_choice: None,
        }
    }

    fn model_caps(supports_audio: bool) -> ModelCapabilities {
        ModelCapabilities {
            max_input_tokens: 1,
            max_output_tokens: 1,
            default_max_output_tokens: 1,
            default_temperature_milli: 0,
            supports_tools: false,
            supports_vision: false,
            supports_audio,
            supports_thinking: false,
            supports_prompt_cache: false,
            reasoning_effort: None,
            supports_web_search: false,
            supports_temperature: true,
        }
    }

    #[test]
    fn text_only_request_passes_without_audio_caps() {
        let req = request(vec![json!({
            "role": "user",
            "content": [{"type": "text", "text": "hi"}],
        })]);

        let result = check_audio_capability(
            &req,
            &ProviderCapabilities::default(),
            &model_caps(false),
            "provider",
            "m",
        );

        result.expect("test result");
    }

    #[test]
    fn audio_request_rejects_missing_provider_cap() {
        let req = request(vec![json!({
            "role": "user",
            "content": [{"type": "audio", "mime": "audio/wav", "data": "AA=="}],
        })]);

        let err = check_audio_capability(
            &req,
            &ProviderCapabilities::default(),
            &model_caps(true),
            "provider",
            "m",
        )
        .expect_err("provider without audio rejects");

        assert!(matches!(err, ProviderError::AudioUnsupported { .. }));
    }
}
