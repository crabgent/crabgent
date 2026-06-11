//! `OpenAI` model catalog returned from `Provider::models()`.
//!
//! Scoped to the models that the Codex Responses backend
//! (`https://chatgpt.com/backend-api/codex/responses`) actually accepts
//! on a `ChatGPT` subscription, verified by probing the endpoint with an
//! `OAuth` bearer. Models the backend rejects with "model is not
//! supported when using Codex with a `ChatGPT` account" are omitted.
//!
//! Pricing, where present, is the official rate-card per-Mtok value mirrored
//! from `openrouter.ai/api/v1/models` (which tracks `OpenAI`'s public price
//! sheet). It applies to API-key access on `api.openai.com`; `ChatGPT`
//! subscription billing is flat-rate and ignores these numbers, but the
//! values keep the catalog meaningful for cost-aware tools that compare
//! providers.

use crabgent_core::{ModelCapabilities, ModelId, ModelInfo, Pricing, ReasoningEffort};
use std::collections::HashMap;

/// Stable provider name reported by `Provider::name`.
pub const PROVIDER: &str = "openai";

pub const GPT_5_5: &str = "gpt-5.5";
pub const GPT_5_4: &str = "gpt-5.4";
pub const GPT_5_4_MINI: &str = "gpt-5.4-mini";
pub const GPT_5_3_CODEX: &str = "gpt-5.3-codex";
pub const GPT_5_3_CODEX_SPARK: &str = "gpt-5.3-codex-spark";
pub const GPT_5_2: &str = "gpt-5.2";
/// Audio-input side-channel models (text + audio in, text out). Routed via
/// the `[audio]` capability config independently of the chat model.
/// `gpt-4o-audio-preview` was retired by `OpenAI` (the public API now answers
/// `404 model_not_found` for it); the successors are `gpt-audio` and
/// `gpt-audio-mini`. The old id is kept as an alias on `gpt_audio()` so
/// existing configs that still name it keep resolving to a working audio
/// model instead of failing the audio pre-flight.
pub const GPT_AUDIO: &str = "gpt-audio";
pub const GPT_AUDIO_MINI: &str = "gpt-audio-mini";
pub const GPT_4O_AUDIO: &str = "gpt-4o-audio-preview";

const FLAGSHIP_INPUT_TOKENS: u32 = 1_050_000;
const MINI_INPUT_TOKENS: u32 = 272_000;
const DEFAULT_OUTPUT_TOKENS: u32 = 128_000;
const DEFAULT_MAX_OUTPUT: u32 = 8_192;
const DEFAULT_TEMPERATURE_MILLI: u16 = 1_000;

/// Build the full catalog of models served by this provider.
#[must_use]
pub fn openai_models() -> Vec<ModelInfo> {
    vec![
        gpt_5_5(),
        gpt_5_4(),
        gpt_5_4_mini(),
        gpt_5_3_codex(),
        gpt_5_3_codex_spark(),
        gpt_5_2(),
        gpt_audio(),
        gpt_audio_mini(),
    ]
}

fn gpt_5_3_codex() -> ModelInfo {
    ModelInfo {
        id: ModelId::new(GPT_5_3_CODEX),
        provider: PROVIDER.into(),
        display_name: "GPT-5.3 Codex".into(),
        aliases: Vec::new(),
        caps: known_llm_caps(MINI_INPUT_TOKENS, DEFAULT_OUTPUT_TOKENS),
        pricing: Some(Pricing {
            input_per_mtok_usd: 1.75,
            output_per_mtok_usd: 14.0,
            cache_read_per_mtok_usd: Some(0.175),
            cache_write_per_mtok_usd: None,
        }),
        extensions: HashMap::new(),
    }
}

fn gpt_5_3_codex_spark() -> ModelInfo {
    ModelInfo {
        id: ModelId::new(GPT_5_3_CODEX_SPARK),
        provider: PROVIDER.into(),
        display_name: "GPT-5.3 Codex Spark".into(),
        aliases: Vec::new(),
        caps: known_llm_caps(MINI_INPUT_TOKENS, 64_000),
        pricing: None,
        extensions: HashMap::new(),
    }
}

#[must_use]
pub fn discovered_model(id: &str) -> ModelInfo {
    known_model(id).unwrap_or_else(|| discovered_unknown_model(id))
}

fn gpt_5_5() -> ModelInfo {
    flagship_template(
        GPT_5_5,
        "GPT-5.5",
        Some(Pricing {
            input_per_mtok_usd: 5.0,
            output_per_mtok_usd: 30.0,
            cache_read_per_mtok_usd: Some(0.5),
            cache_write_per_mtok_usd: None,
        }),
        true,
    )
}

fn gpt_5_4() -> ModelInfo {
    flagship_template(
        GPT_5_4,
        "GPT-5.4",
        Some(Pricing {
            input_per_mtok_usd: 2.5,
            output_per_mtok_usd: 15.0,
            cache_read_per_mtok_usd: Some(0.25),
            cache_write_per_mtok_usd: None,
        }),
        true,
    )
}

fn gpt_5_4_mini() -> ModelInfo {
    ModelInfo {
        id: ModelId::new(GPT_5_4_MINI),
        provider: PROVIDER.into(),
        display_name: "GPT-5.4 mini".into(),
        aliases: Vec::new(),
        caps: known_llm_caps(MINI_INPUT_TOKENS, 64_000),
        pricing: Some(Pricing {
            input_per_mtok_usd: 0.75,
            output_per_mtok_usd: 4.5,
            cache_read_per_mtok_usd: Some(0.075),
            cache_write_per_mtok_usd: None,
        }),
        extensions: HashMap::new(),
    }
}

fn gpt_5_2() -> ModelInfo {
    ModelInfo {
        id: ModelId::new(GPT_5_2),
        provider: PROVIDER.into(),
        display_name: "GPT-5.2".into(),
        aliases: Vec::new(),
        caps: web_search_llm_caps(MINI_INPUT_TOKENS, DEFAULT_OUTPUT_TOKENS, true),
        pricing: Some(Pricing {
            input_per_mtok_usd: 1.75,
            output_per_mtok_usd: 14.0,
            cache_read_per_mtok_usd: Some(0.175),
            cache_write_per_mtok_usd: None,
        }),
        extensions: HashMap::new(),
    }
}

fn gpt_audio() -> ModelInfo {
    ModelInfo {
        id: ModelId::new(GPT_AUDIO),
        provider: PROVIDER.into(),
        display_name: "GPT Audio".into(),
        // Carry the retired `gpt-4o-audio-preview` id as an alias so configs
        // and persisted overrides that still name it resolve here.
        aliases: vec![ModelId::new(GPT_4O_AUDIO)],
        caps: audio_llm_caps(128_000, 16_384),
        pricing: Some(Pricing {
            input_per_mtok_usd: 2.5,
            output_per_mtok_usd: 10.0,
            cache_read_per_mtok_usd: None,
            cache_write_per_mtok_usd: None,
        }),
        extensions: HashMap::new(),
    }
}

fn gpt_audio_mini() -> ModelInfo {
    ModelInfo {
        id: ModelId::new(GPT_AUDIO_MINI),
        provider: PROVIDER.into(),
        display_name: "GPT Audio Mini".into(),
        aliases: Vec::new(),
        caps: audio_llm_caps(128_000, 16_384),
        pricing: Some(Pricing {
            input_per_mtok_usd: 0.6,
            output_per_mtok_usd: 2.4,
            cache_read_per_mtok_usd: None,
            cache_write_per_mtok_usd: None,
        }),
        extensions: HashMap::new(),
    }
}

fn flagship_template(
    id: &str,
    display: &str,
    pricing: Option<Pricing>,
    web_search: bool,
) -> ModelInfo {
    ModelInfo {
        id: ModelId::new(id),
        provider: PROVIDER.into(),
        display_name: display.into(),
        aliases: Vec::new(),
        caps: web_search_llm_caps(FLAGSHIP_INPUT_TOKENS, DEFAULT_OUTPUT_TOKENS, web_search),
        pricing,
        extensions: HashMap::new(),
    }
}

fn known_model(id: &str) -> Option<ModelInfo> {
    match id {
        GPT_5_5 => Some(gpt_5_5()),
        GPT_5_4 => Some(gpt_5_4()),
        GPT_5_4_MINI => Some(gpt_5_4_mini()),
        GPT_5_3_CODEX => Some(gpt_5_3_codex()),
        GPT_5_3_CODEX_SPARK => Some(gpt_5_3_codex_spark()),
        GPT_5_2 => Some(gpt_5_2()),
        GPT_AUDIO | GPT_4O_AUDIO => Some(gpt_audio()),
        GPT_AUDIO_MINI => Some(gpt_audio_mini()),
        _ => None,
    }
}

const fn known_llm_caps(max_input_tokens: u32, max_output_tokens: u32) -> ModelCapabilities {
    web_search_llm_caps(max_input_tokens, max_output_tokens, false)
}

const fn web_search_llm_caps(
    max_input_tokens: u32,
    max_output_tokens: u32,
    web_search: bool,
) -> ModelCapabilities {
    ModelCapabilities {
        max_input_tokens,
        max_output_tokens,
        default_max_output_tokens: DEFAULT_MAX_OUTPUT,
        default_temperature_milli: DEFAULT_TEMPERATURE_MILLI,
        supports_tools: true,
        supports_vision: true,
        supports_audio: false,
        supports_thinking: true,
        supports_prompt_cache: true,
        reasoning_effort: Some(ReasoningEffort::Low),
        supports_web_search: web_search,
        supports_temperature: true,
    }
}

/// Capabilities for the audio-input side-channel model: audio in, no
/// vision, no reasoning effort, tools allowed.
const fn audio_llm_caps(max_input_tokens: u32, max_output_tokens: u32) -> ModelCapabilities {
    ModelCapabilities {
        max_input_tokens,
        max_output_tokens,
        default_max_output_tokens: DEFAULT_MAX_OUTPUT,
        default_temperature_milli: DEFAULT_TEMPERATURE_MILLI,
        supports_tools: true,
        supports_vision: false,
        supports_audio: true,
        supports_thinking: false,
        supports_prompt_cache: false,
        reasoning_effort: None,
        supports_web_search: false,
        supports_temperature: true,
    }
}

fn discovered_unknown_model(id: &str) -> ModelInfo {
    ModelInfo {
        id: ModelId::new(id),
        provider: PROVIDER.into(),
        display_name: id.into(),
        aliases: Vec::new(),
        caps: ModelCapabilities {
            max_input_tokens: 200_000,
            max_output_tokens: DEFAULT_OUTPUT_TOKENS,
            default_max_output_tokens: DEFAULT_MAX_OUTPUT,
            default_temperature_milli: DEFAULT_TEMPERATURE_MILLI,
            supports_tools: false,
            supports_vision: false,
            supports_audio: false,
            supports_thinking: false,
            supports_prompt_cache: false,
            reasoning_effort: None,
            supports_web_search: false,
            supports_temperature: true,
        },
        pricing: None,
        extensions: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lists_codex_supported_models() {
        let catalog = openai_models();
        let ids: Vec<&str> = catalog.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&GPT_5_5));
        assert!(ids.contains(&GPT_5_4));
        assert!(ids.contains(&GPT_5_4_MINI));
        assert!(ids.contains(&GPT_5_3_CODEX));
        assert!(ids.contains(&GPT_5_3_CODEX_SPARK));
        assert!(ids.contains(&GPT_5_2));
        assert!(!ids.contains(&"gpt-5.5-codex"));
    }

    #[test]
    fn every_model_has_openai_provider() {
        for model in openai_models() {
            assert_eq!(model.provider, PROVIDER);
        }
    }

    #[test]
    fn every_model_advertises_plan_capabilities() {
        for model in openai_models() {
            // The audio side-channel models carry a distinct profile.
            if model.caps.supports_audio {
                continue;
            }
            assert!(model.caps.supports_vision);
            assert!(model.caps.supports_tools);
            assert!(model.caps.supports_prompt_cache);
            assert!(model.caps.supports_thinking);
            assert!(!model.caps.supports_audio);
        }
    }

    #[test]
    fn discovered_known_model_uses_catalog_capabilities() {
        let discovered = discovered_model(GPT_5_5);
        let catalog = gpt_5_5();

        assert_eq!(discovered, catalog);
    }

    #[test]
    fn discovered_unknown_model_is_conservative() {
        let model = discovered_model("gpt-future");

        assert_eq!(model.id.as_str(), "gpt-future");
        assert_eq!(model.provider, PROVIDER);
        assert!(!model.caps.supports_vision);
        assert!(!model.caps.supports_tools);
        assert!(!model.caps.supports_prompt_cache);
        assert!(!model.caps.supports_thinking);
        assert!(!model.caps.supports_audio);
        assert_eq!(model.caps.reasoning_effort, None);
        assert!(model.pricing.is_none());
    }

    #[test]
    fn every_model_advertises_reasoning_effort_default() {
        for model in openai_models() {
            // The audio side-channel models are not reasoning models.
            if model.caps.supports_audio {
                continue;
            }
            assert_eq!(
                model.caps.reasoning_effort,
                Some(ReasoningEffort::Low),
                "expected Low default for {}",
                model.id.as_str()
            );
        }
    }

    #[test]
    fn audio_model_advertises_audio_and_no_vision() {
        let model = openai_models()
            .into_iter()
            .find(|m| m.id.as_str() == GPT_AUDIO)
            .expect("audio model present in catalog");
        assert!(model.caps.supports_audio);
        assert!(!model.caps.supports_vision);
        assert!(model.caps.supports_tools);
        assert_eq!(model.caps.reasoning_effort, None);
        assert!(discovered_model(GPT_AUDIO).caps.supports_audio);
    }

    #[test]
    fn gpt_audio_mini_advertises_audio() {
        let model = openai_models()
            .into_iter()
            .find(|m| m.id.as_str() == GPT_AUDIO_MINI)
            .expect("audio-mini model present in catalog");
        assert!(model.caps.supports_audio);
        assert!(!model.caps.supports_vision);
    }

    #[test]
    fn retired_audio_id_aliases_to_gpt_audio() {
        // OpenAI removed gpt-4o-audio-preview; configs that still name it must
        // resolve to the working audio model so the audio pre-flight passes.
        let model = discovered_model(GPT_4O_AUDIO);
        assert_eq!(model.id.as_str(), GPT_AUDIO);
        assert!(model.caps.supports_audio);
    }
}
