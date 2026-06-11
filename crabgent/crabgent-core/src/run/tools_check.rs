//! Tool capability pre-flight before provider calls.

use crate::error::ProviderError;
use crate::model::ModelCapabilities;
use crate::provider::ProviderCapabilities;
use crate::types::LlmRequest;

pub(super) fn check_tools_capability(
    req: &LlmRequest,
    provider_caps: &ProviderCapabilities,
    model_caps: &ModelCapabilities,
    provider_name: &str,
    model_id: &str,
) -> Result<(), ProviderError> {
    if req.tools.is_empty() {
        return Ok(());
    }

    if provider_caps.tools && model_caps.supports_tools {
        return Ok(());
    }

    Err(ProviderError::ToolsUnsupported {
        provider: provider_name.to_owned(),
        model: model_id.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::types::ToolDef;

    fn request(tools: Vec<ToolDef>) -> LlmRequest {
        LlmRequest {
            model: "m".into(),
            system_prompt: None,
            messages: vec![json!({"role": "user", "content": "hi"})],
            tools,
            max_tokens: None,
            temperature: None,
            stop_sequences: Vec::new(),
            reasoning_effort: None,
            web_search: crate::types::WebSearchConfig::default(),
            tool_choice: None,
        }
    }

    fn tool_def() -> ToolDef {
        ToolDef {
            name: "noop".into(),
            description: "stub".into(),
            input_schema: json!({"type": "object"}),
        }
    }

    fn model_caps(supports_tools: bool) -> ModelCapabilities {
        ModelCapabilities {
            max_input_tokens: 1,
            max_output_tokens: 1,
            default_max_output_tokens: 1,
            default_temperature_milli: 0,
            supports_tools,
            supports_vision: false,
            supports_audio: false,
            supports_thinking: false,
            supports_prompt_cache: false,
            reasoning_effort: None,
            supports_web_search: false,
            supports_temperature: true,
        }
    }

    #[test]
    fn no_tools_request_passes_without_tool_caps() {
        let result = check_tools_capability(
            &request(Vec::new()),
            &ProviderCapabilities::default(),
            &model_caps(false),
            "provider",
            "m",
        );

        result.expect("no tools should not require tool caps");
    }

    #[test]
    fn tools_request_rejects_missing_provider_cap() {
        let err = check_tools_capability(
            &request(vec![tool_def()]),
            &ProviderCapabilities::default(),
            &model_caps(true),
            "provider",
            "m",
        )
        .expect_err("provider without tools rejects");

        assert!(matches!(err, ProviderError::ToolsUnsupported { .. }));
    }
}
