//! Google Gemini model catalogs.

use std::collections::HashMap;

use crabgent_core::{
    ImageGenerationModelId, ImageGenerationModelInfo, ModelCapabilities, ModelId, ModelInfo,
    ReasoningEffort,
};

/// Stable provider name reported by `Provider::name`.
pub const PROVIDER: &str = "google";

pub const GEMINI_3_PRO_PREVIEW: &str = "gemini-3-pro-preview";
pub const GEMINI_3_FLASH_PREVIEW: &str = "gemini-3-flash-preview";
pub const GEMINI_3_1_PRO_PREVIEW: &str = "gemini-3.1-pro-preview";
pub const GEMINI_3_1_FLASH_LITE: &str = "gemini-3.1-flash-lite";
pub const GEMINI_3_5_FLASH: &str = "gemini-3.5-flash";
pub const GEMINI_2_5_PRO: &str = "gemini-2.5-pro";
pub const GEMINI_2_5_FLASH: &str = "gemini-2.5-flash";
pub const GEMINI_2_5_FLASH_LITE: &str = "gemini-2.5-flash-lite";
pub const GEMINI_2_0_FLASH: &str = "gemini-2.0-flash";
pub const GEMINI_2_0_FLASH_LITE: &str = "gemini-2.0-flash-lite";

pub const GEMINI_3_1_FLASH_IMAGE_PREVIEW: &str = "gemini-3.1-flash-image-preview";
pub const GEMINI_3_PRO_IMAGE_PREVIEW: &str = "gemini-3-pro-image-preview";

const GEMINI_INPUT_TOKENS: u32 = 1_000_000;
const GEMINI_OUTPUT_TOKENS: u32 = 65_536;
const DEFAULT_MAX_OUTPUT: u32 = 8_192;

/// Canonical `(id, display_name)` pairs for the Gemini chat catalog. Single
/// source for both `google_models` and the known-id branch of `discovered_model`.
const GEMINI_CHAT_MODELS: &[(&str, &str)] = &[
    (GEMINI_3_PRO_PREVIEW, "Gemini 3 Pro Preview"),
    (GEMINI_3_FLASH_PREVIEW, "Gemini 3 Flash Preview"),
    (GEMINI_3_1_PRO_PREVIEW, "Gemini 3.1 Pro Preview"),
    (GEMINI_3_1_FLASH_LITE, "Gemini 3.1 Flash Lite"),
    (GEMINI_3_5_FLASH, "Gemini 3.5 Flash"),
    (GEMINI_2_5_PRO, "Gemini 2.5 Pro"),
    (GEMINI_2_5_FLASH, "Gemini 2.5 Flash"),
    (GEMINI_2_5_FLASH_LITE, "Gemini 2.5 Flash Lite"),
    (GEMINI_2_0_FLASH, "Gemini 2.0 Flash"),
    (GEMINI_2_0_FLASH_LITE, "Gemini 2.0 Flash Lite"),
];

/// Build the Gemini chat catalog.
#[must_use]
pub fn google_models() -> Vec<ModelInfo> {
    GEMINI_CHAT_MODELS
        .iter()
        .map(|&(id, display_name)| gemini_model(id, display_name))
        .collect()
}

#[must_use]
pub fn discovered_model(id: &str) -> ModelInfo {
    GEMINI_CHAT_MODELS
        .iter()
        .find(|&&(known, _)| known == id)
        .map_or_else(
            || discovered_unknown_model(id),
            |&(known, display_name)| gemini_model(known, display_name),
        )
}

/// Build the Google image-generation catalog.
#[must_use]
pub fn google_image_generation_models() -> Vec<ImageGenerationModelInfo> {
    vec![
        image_model(
            GEMINI_3_1_FLASH_IMAGE_PREVIEW,
            "Gemini 3.1 Flash Image Preview",
        ),
        image_model(GEMINI_3_PRO_IMAGE_PREVIEW, "Gemini 3 Pro Image Preview"),
    ]
}

fn gemini_model(id: &'static str, display_name: &'static str) -> ModelInfo {
    ModelInfo {
        id: ModelId::new(id),
        provider: PROVIDER.into(),
        display_name: display_name.into(),
        aliases: vec![ModelId::new(format!("models/{id}"))],
        caps: gemini_caps(gemini_model_features(id)),
        pricing: None,
        extensions: HashMap::new(),
    }
}

#[derive(Clone, Copy)]
struct GeminiModelFeatures {
    audio: bool,
    thinking: bool,
    prompt_cache: bool,
    reasoning_effort: Option<ReasoningEffort>,
}

fn gemini_model_features(id: &str) -> GeminiModelFeatures {
    let thinking = model_id_supports_thinking(id);
    GeminiModelFeatures {
        audio: model_id_supports_audio(id),
        thinking,
        prompt_cache: model_id_supports_prompt_cache(id),
        reasoning_effort: if thinking {
            Some(ReasoningEffort::Medium)
        } else {
            None
        },
    }
}

fn model_id_supports_thinking(id: &str) -> bool {
    id.starts_with("gemini-2.5") || id.starts_with("gemini-3")
}

fn model_id_supports_audio(id: &str) -> bool {
    id.starts_with("gemini-2.0") || id.starts_with("gemini-2.5") || id.starts_with("gemini-3")
}

fn model_id_supports_prompt_cache(id: &str) -> bool {
    id.starts_with("gemini-2.0") || id.starts_with("gemini-2.5") || id.starts_with("gemini-3")
}

const fn gemini_caps(features: GeminiModelFeatures) -> ModelCapabilities {
    ModelCapabilities {
        max_input_tokens: GEMINI_INPUT_TOKENS,
        max_output_tokens: GEMINI_OUTPUT_TOKENS,
        default_max_output_tokens: DEFAULT_MAX_OUTPUT,
        default_temperature_milli: 1_000,
        supports_tools: true,
        supports_vision: true,
        supports_audio: features.audio,
        supports_thinking: features.thinking,
        supports_prompt_cache: features.prompt_cache,
        reasoning_effort: features.reasoning_effort,
        supports_web_search: true,
        supports_temperature: true,
    }
}

fn discovered_unknown_model(id: &str) -> ModelInfo {
    ModelInfo {
        id: ModelId::new(id),
        provider: PROVIDER.into(),
        display_name: id.into(),
        aliases: Vec::new(),
        caps: gemini_caps(gemini_model_features(id)),
        pricing: None,
        extensions: HashMap::new(),
    }
}

fn image_model(id: &'static str, display_name: &'static str) -> ImageGenerationModelInfo {
    ImageGenerationModelInfo {
        id: ImageGenerationModelId::new(id),
        display_name: display_name.to_owned(),
        supports_editing: false,
        supports_transparent_background: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lists_gemini_chat_families() {
        let models = google_models();
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert!(models.len() >= 8);
        assert!(ids.iter().any(|id| id.starts_with("gemini-2.0")));
        assert!(ids.iter().any(|id| id.starts_with("gemini-2.5")));
        assert!(ids.iter().any(|id| id.starts_with("gemini-3-")));
        assert!(ids.iter().any(|id| id.starts_with("gemini-3.1")));
        assert!(ids.iter().any(|id| id.starts_with("gemini-3.5")));
        assert!(ids.contains(&GEMINI_3_5_FLASH));
        for model in &models {
            assert!(model.caps.supports_tools);
            assert!(model.caps.supports_vision);
            assert!(model.caps.supports_audio);
            assert!(model.caps.supports_prompt_cache);
            assert!(model.caps.supports_web_search);
        }
    }

    #[test]
    fn catalog_marks_thinking_models_per_family() {
        let thinking = discovered_model(GEMINI_2_5_FLASH);
        assert!(thinking.caps.supports_thinking);
        assert_eq!(
            thinking.caps.reasoning_effort,
            Some(ReasoningEffort::Medium)
        );

        let non_thinking = discovered_model(GEMINI_2_0_FLASH);
        assert!(!non_thinking.caps.supports_thinking);
        assert_eq!(non_thinking.caps.reasoning_effort, None);
    }

    #[test]
    fn gemini_3_5_flash_supports_web_search() {
        let model = discovered_model(GEMINI_3_5_FLASH);
        assert!(model.caps.supports_web_search);
    }

    #[test]
    fn image_catalog_lists_gemini_image_models() {
        let models = google_image_generation_models();
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert!(ids.contains(&GEMINI_3_1_FLASH_IMAGE_PREVIEW));
        assert!(ids.contains(&GEMINI_3_PRO_IMAGE_PREVIEW));
    }
}
