//! Anthropic model catalog returned from `Provider::models()`.
//!
//! Each entry pins the canonical id (`claude-haiku-4-5` etc.), the
//! per-model capability flags, and the dated alias the API also
//! accepts. Pricing values come from the public Anthropic price-list
//! and are USD per million tokens; treat them as a hint, not a
//! contract (the price-list changes faster than this catalog).

use crabgent_core::{ModelCapabilities, ModelId, ModelInfo, Pricing};

/// Stable provider name reported by `Provider::name`.
pub const PROVIDER: &str = "anthropic";

const ANTHROPIC_INPUT_TOKENS: u32 = 200_000;

/// Build the full catalog of models served by this provider.
///
/// The function allocates fresh `ModelInfo`s on every call. Cost is
/// negligible (the kernel calls it once during `KernelBuilder::build`).
#[must_use]
pub fn anthropic_models() -> Vec<ModelInfo> {
    vec![
        haiku_4_5(),
        sonnet_4_5(),
        sonnet_4_6(),
        opus_4_5(),
        opus_4_6(),
        opus_4_7(),
        opus_4_8(),
    ]
}

/// Per-model fields that vary across the otherwise-uniform Anthropic catalog.
///
/// Shared defaults (provider, the 200k input window, `default_temperature_milli`,
/// tool/vision support, no audio, no thinking, prompt-cache support) live in
/// `anthropic_model`; only the fields that actually differ are listed here.
struct AnthropicModelSpec {
    id: &'static str,
    display_name: &'static str,
    aliases: Vec<ModelId>,
    max_output_tokens: u32,
    default_max_output_tokens: u32,
    supports_web_search: bool,
    supports_temperature: bool,
    pricing: Pricing,
}

/// Build a `ModelInfo` from the shared Anthropic template plus the per-model
/// `spec`. Models with a divergent input context (Opus 4.8) are built inline.
fn anthropic_model(spec: AnthropicModelSpec) -> ModelInfo {
    ModelInfo {
        id: ModelId::new(spec.id),
        provider: PROVIDER.into(),
        display_name: spec.display_name.into(),
        aliases: spec.aliases,
        caps: ModelCapabilities {
            max_input_tokens: ANTHROPIC_INPUT_TOKENS,
            max_output_tokens: spec.max_output_tokens,
            default_max_output_tokens: spec.default_max_output_tokens,
            default_temperature_milli: 1_000,
            supports_tools: true,
            supports_vision: true,
            supports_audio: false,
            supports_thinking: false,
            supports_prompt_cache: true,
            reasoning_effort: None,
            supports_web_search: spec.supports_web_search,
            supports_temperature: spec.supports_temperature,
        },
        pricing: Some(spec.pricing),
        extensions: std::collections::HashMap::new(),
    }
}

fn haiku_4_5() -> ModelInfo {
    anthropic_model(AnthropicModelSpec {
        id: "claude-haiku-4-5",
        display_name: "Claude Haiku 4.5",
        aliases: vec![ModelId::new("claude-haiku-4-5-20251001")],
        max_output_tokens: 8_192,
        default_max_output_tokens: 4_096,
        supports_web_search: false,
        supports_temperature: true,
        pricing: Pricing {
            input_per_mtok_usd: 1.0,
            output_per_mtok_usd: 5.0,
            cache_read_per_mtok_usd: None,
            cache_write_per_mtok_usd: None,
        },
    })
}

fn sonnet_4_6() -> ModelInfo {
    anthropic_model(AnthropicModelSpec {
        id: "claude-sonnet-4-6",
        display_name: "Claude Sonnet 4.6",
        aliases: vec![ModelId::new("claude-sonnet-4-6-20251201")],
        max_output_tokens: 64_000,
        default_max_output_tokens: 8_192,
        supports_web_search: true,
        supports_temperature: true,
        pricing: Pricing {
            input_per_mtok_usd: 3.0,
            output_per_mtok_usd: 15.0,
            cache_read_per_mtok_usd: None,
            cache_write_per_mtok_usd: None,
        },
    })
}

fn sonnet_4_5() -> ModelInfo {
    anthropic_model(AnthropicModelSpec {
        id: "claude-sonnet-4-5",
        display_name: "Claude Sonnet 4.5",
        aliases: vec![ModelId::new("claude-sonnet-4-5-20250929")],
        max_output_tokens: 64_000,
        default_max_output_tokens: 8_192,
        supports_web_search: false,
        supports_temperature: true,
        pricing: Pricing {
            input_per_mtok_usd: 3.0,
            output_per_mtok_usd: 15.0,
            cache_read_per_mtok_usd: Some(0.3),
            cache_write_per_mtok_usd: Some(3.75),
        },
    })
}

fn opus_4_5() -> ModelInfo {
    anthropic_model(AnthropicModelSpec {
        id: "claude-opus-4-5",
        display_name: "Claude Opus 4.5",
        aliases: vec![ModelId::new("claude-opus-4-5-20251101")],
        max_output_tokens: 32_000,
        default_max_output_tokens: 8_192,
        supports_web_search: false,
        supports_temperature: true,
        pricing: Pricing {
            input_per_mtok_usd: 15.0,
            output_per_mtok_usd: 75.0,
            cache_read_per_mtok_usd: Some(1.5),
            cache_write_per_mtok_usd: Some(18.75),
        },
    })
}

fn opus_4_6() -> ModelInfo {
    anthropic_model(AnthropicModelSpec {
        id: "claude-opus-4-6",
        display_name: "Claude Opus 4.6",
        aliases: Vec::new(),
        max_output_tokens: 32_000,
        default_max_output_tokens: 8_192,
        supports_web_search: true,
        supports_temperature: true,
        pricing: Pricing {
            input_per_mtok_usd: 15.0,
            output_per_mtok_usd: 75.0,
            cache_read_per_mtok_usd: Some(1.5),
            cache_write_per_mtok_usd: Some(18.75),
        },
    })
}

fn opus_4_7() -> ModelInfo {
    // Opus 4.7 dropped the `temperature` API parameter; any non-default
    // value returns HTTP 400 with "temperature is deprecated for this
    // model" (Anthropic migration guide, 2026). Callers must omit the
    // field entirely; the compact hook uses
    // `ModelCapabilities::supports_temperature` to short-circuit.
    anthropic_model(AnthropicModelSpec {
        id: "claude-opus-4-7",
        display_name: "Claude Opus 4.7",
        aliases: vec![ModelId::new("claude-opus-4-7-20260115")],
        max_output_tokens: 32_000,
        default_max_output_tokens: 8_192,
        supports_web_search: true,
        supports_temperature: false,
        pricing: Pricing {
            input_per_mtok_usd: 15.0,
            output_per_mtok_usd: 75.0,
            cache_read_per_mtok_usd: None,
            cache_write_per_mtok_usd: None,
        },
    })
}

fn opus_4_8() -> ModelInfo {
    // Opus 4.8 keeps the temperature restriction introduced with 4.7 and adds
    // a documented 1M-token input context plus a 128k output ceiling. The
    // divergent input window means it cannot go through `anthropic_model`
    // (which pins the standard 200k context), so it is built inline. We have
    // no confirmed dated snapshot id yet, so aliases stays empty.
    ModelInfo {
        id: ModelId::new("claude-opus-4-8"),
        provider: PROVIDER.into(),
        display_name: "Claude Opus 4.8".into(),
        aliases: vec![],
        caps: ModelCapabilities {
            max_input_tokens: 1_000_000,
            max_output_tokens: 128_000,
            default_max_output_tokens: 8_192,
            default_temperature_milli: 1_000,
            supports_tools: true,
            supports_vision: true,
            supports_audio: false,
            supports_thinking: false,
            supports_prompt_cache: true,
            reasoning_effort: None,
            supports_web_search: true,
            supports_temperature: false,
        },
        pricing: Some(Pricing {
            input_per_mtok_usd: 15.0,
            output_per_mtok_usd: 75.0,
            cache_read_per_mtok_usd: None,
            cache_write_per_mtok_usd: None,
        }),
        extensions: std::collections::HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_seven_entries() {
        let models = anthropic_models();
        assert_eq!(models.len(), 7);
    }

    #[test]
    fn ids_are_unique_across_canonicals_and_aliases() {
        let models = anthropic_models();
        let mut seen: Vec<&str> = Vec::new();
        for m in &models {
            seen.push(m.id.as_str());
            for a in &m.aliases {
                seen.push(a.as_str());
            }
        }
        let initial = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), initial, "duplicate id in anthropic_models");
    }

    #[test]
    fn every_model_has_anthropic_provider() {
        for m in anthropic_models() {
            assert_eq!(m.provider, PROVIDER);
        }
    }

    #[test]
    fn every_model_has_pricing() {
        for m in anthropic_models() {
            assert!(m.pricing.is_some(), "missing pricing for {}", m.id);
        }
    }

    #[test]
    fn haiku_does_not_advertise_thinking() {
        let h = haiku_4_5();
        assert!(!h.caps.supports_thinking);
    }

    #[test]
    fn sonnet_and_opus_do_not_advertise_thinking_without_request_support() {
        assert!(!sonnet_4_6().caps.supports_thinking);
        assert!(!opus_4_7().caps.supports_thinking);
        assert!(!opus_4_8().caps.supports_thinking);
    }

    #[test]
    fn opus_4_7_does_not_support_temperature() {
        assert!(
            !opus_4_7().caps.supports_temperature,
            "opus-4-7 rejects the temperature API parameter (HTTP 400)",
        );
    }

    #[test]
    fn opus_4_8_does_not_support_temperature() {
        assert!(
            !opus_4_8().caps.supports_temperature,
            "opus-4-8 rejects the temperature API parameter (HTTP 400)",
        );
    }

    #[test]
    fn opus_4_8_advertises_documented_context_and_output() {
        let m = opus_4_8();
        assert_eq!(m.caps.max_input_tokens, 1_000_000);
        assert_eq!(m.caps.max_output_tokens, 128_000);
    }

    #[test]
    fn non_recent_opus_models_still_support_temperature() {
        for m in anthropic_models() {
            if matches!(m.id.as_str(), "claude-opus-4-7" | "claude-opus-4-8") {
                continue;
            }
            assert!(
                m.caps.supports_temperature,
                "{} should still advertise temperature support",
                m.id,
            );
        }
    }

    #[test]
    fn non_opus_4_8_input_tokens_are_uniform() {
        // Opus 4.8 carries a documented 1M-token context and is exempt; every
        // other model shares the standard 200k input window.
        for m in anthropic_models() {
            if m.id.as_str() == "claude-opus-4-8" {
                continue;
            }
            assert_eq!(m.caps.max_input_tokens, ANTHROPIC_INPUT_TOKENS);
        }
    }

    #[test]
    fn default_temperature_is_one() {
        for m in anthropic_models() {
            assert!((m.caps.default_temperature() - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn pricing_output_higher_than_input() {
        for m in anthropic_models() {
            let p = m.pricing.expect("pricing");
            assert!(p.output_per_mtok_usd > p.input_per_mtok_usd);
        }
    }
}
