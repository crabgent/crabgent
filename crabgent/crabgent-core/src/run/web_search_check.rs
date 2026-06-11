//! Web-search capability pre-flight before provider calls.

use crate::error::ProviderError;
use crate::model::ModelCapabilities;
use crate::provider::ProviderCapabilities;
use crate::types::LlmRequest;

pub(super) fn check_web_search_capability(
    req: &LlmRequest,
    provider_caps: &ProviderCapabilities,
    model_caps: &ModelCapabilities,
    provider_name: &str,
    model_id: &str,
) -> Result<(), ProviderError> {
    if !req.web_search.enabled {
        return Ok(());
    }

    if provider_caps.web_search && model_caps.supports_web_search {
        return Ok(());
    }

    Err(ProviderError::WebSearchUnsupported {
        provider: provider_name.to_owned(),
        model: model_id.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::WebSearchConfig;

    fn request_with_web_search(enabled: bool) -> LlmRequest {
        LlmRequest {
            model: "m".into(),
            system_prompt: None,
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            stop_sequences: Vec::new(),
            reasoning_effort: None,
            web_search: WebSearchConfig {
                enabled,
                ..WebSearchConfig::default()
            },
            tool_choice: None,
        }
    }

    fn model_caps(supports_web_search: bool) -> ModelCapabilities {
        ModelCapabilities {
            max_input_tokens: 1,
            max_output_tokens: 1,
            default_max_output_tokens: 1,
            default_temperature_milli: 0,
            supports_tools: false,
            supports_vision: false,
            supports_audio: false,
            supports_thinking: false,
            supports_prompt_cache: false,
            supports_web_search,
            supports_temperature: true,
            reasoning_effort: None,
        }
    }

    fn provider_caps_with_web_search(web_search: bool) -> ProviderCapabilities {
        ProviderCapabilities {
            web_search,
            ..ProviderCapabilities::default()
        }
    }

    #[test]
    fn disabled_request_always_passes() {
        let req = request_with_web_search(false);

        // Even with no caps at all, disabled request passes.
        check_web_search_capability(
            &req,
            &ProviderCapabilities::default(),
            &model_caps(false),
            "provider",
            "m",
        )
        .expect("disabled web_search should always pass");
    }

    #[test]
    fn enabled_request_passes_when_both_caps_set() {
        let req = request_with_web_search(true);

        check_web_search_capability(
            &req,
            &provider_caps_with_web_search(true),
            &model_caps(true),
            "provider",
            "m",
        )
        .expect("enabled web_search with full caps should pass");
    }

    #[test]
    fn enabled_request_rejects_when_provider_cap_missing() {
        let req = request_with_web_search(true);

        let err = check_web_search_capability(
            &req,
            &ProviderCapabilities::default(), // web_search = false
            &model_caps(true),
            "provider",
            "m",
        )
        .expect_err("missing provider cap should reject");

        assert!(matches!(err, ProviderError::WebSearchUnsupported { .. }));
    }

    #[test]
    fn enabled_request_rejects_when_model_cap_missing() {
        let req = request_with_web_search(true);

        let err = check_web_search_capability(
            &req,
            &provider_caps_with_web_search(true),
            &model_caps(false), // supports_web_search = false
            "provider",
            "m",
        )
        .expect_err("missing model cap should reject");

        assert!(matches!(err, ProviderError::WebSearchUnsupported { .. }));
    }

    #[test]
    fn enabled_request_rejects_when_both_caps_missing() {
        let req = request_with_web_search(true);

        let err = check_web_search_capability(
            &req,
            &ProviderCapabilities::default(),
            &model_caps(false),
            "anthropic",
            "claude-3",
        )
        .expect_err("no caps should reject");

        assert!(
            matches!(&err, ProviderError::WebSearchUnsupported { provider, model }
                if provider == "anthropic" && model == "claude-3")
        );
    }
}
