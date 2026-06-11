//! `ModelInfo`: metadata describing a single registered model.
//!
//! Provider-impls return a `Vec<ModelInfo>` from `Provider::models()`;
//! the kernel collects these into a [`ModelRegistry`] at build time.
//!
//! [`ModelRegistry`]: crate::model::registry::ModelRegistry

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::model::capability::{ModelCapability, ReasoningEffort};
use crate::model::id::ModelId;

/// Per-model capability flags and limits.
///
/// `default_max_output_tokens` may be lower than `max_output_tokens`:
/// the cap is what the API technically accepts, the default is what
/// the kernel applies when the request does not pin `max_tokens` itself.
// The boolean flags (tools/vision/audio/thinking/prompt_cache/web_search/
// temperature) are intentionally independent fields: each tracks a
// distinct advertised provider feature, packing them into bitflags would
// just hide structure. Mirrors the pattern in `ProviderCapabilities`.
#[expect(
    clippy::struct_excessive_bools,
    reason = "model capabilities are independent advertised provider features"
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
    pub default_max_output_tokens: u32,
    /// Stored as integer scaled by 1000 so the struct stays `Eq`-able
    /// (no float-NaN). 0..=2000 maps to 0.0..=2.0 in the API.
    pub default_temperature_milli: u16,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_audio: bool,
    pub supports_thinking: bool,
    pub supports_prompt_cache: bool,
    /// Model supports hosted web-search capability.
    ///
    /// Note: no `#[serde(default)]` here because existing registry literal
    /// sites are updated by the bulk-edit script to include this field
    /// explicitly, keeping the registry source-of-truth readable.
    pub supports_web_search: bool,
    /// Model accepts the `temperature` API parameter.
    ///
    /// Set to `false` for models that reject `temperature` outright
    /// (Anthropic opus-4-7 returns HTTP 400 for any value). Callers
    /// that build wire bodies short-circuit before sending. As with
    /// `supports_web_search`, registry literal sites set this field
    /// explicitly instead of relying on `#[serde(default)]`.
    pub supports_temperature: bool,
    /// Per-model default for `reasoning_effort` on Responses + Chat Completions.
    ///
    /// `None` skips the body field entirely (legacy + non-reasoning models);
    /// `Some` emits `reasoning_effort` on Chat Completions and the
    /// `reasoning = {"effort": _, "summary": "auto"}` object on the Codex
    /// Responses path. Populated from the per-provider registry, overridable
    /// on `LlmRequest` per call.
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl ModelCapabilities {
    /// Convenience: fetch the default temperature as `f32` for API calls.
    #[must_use]
    pub fn default_temperature(&self) -> f32 {
        f32::from(self.default_temperature_milli) / 1000.0
    }
}

/// Optional pricing (USD per million tokens).
///
/// Pricing data ages quickly; treat the values as a hint, not a
/// contract. Cost-tracking hooks read this for budget accounting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input_per_mtok_usd: f64,
    pub output_per_mtok_usd: f64,
    pub cache_read_per_mtok_usd: Option<f64>,
    pub cache_write_per_mtok_usd: Option<f64>,
}

/// Full descriptor of a model registered with a provider.
#[derive(Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: ModelId,
    pub provider: String,
    pub display_name: String,
    pub aliases: Vec<ModelId>,
    pub caps: ModelCapabilities,
    pub pricing: Option<Pricing>,
    #[serde(skip)]
    pub extensions: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl fmt::Debug for ModelInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelInfo")
            .field("id", &self.id)
            .field("provider", &self.provider)
            .field("display_name", &self.display_name)
            .field("aliases", &self.aliases)
            .field("caps", &self.caps)
            .field("pricing", &self.pricing)
            .field("extensions_len", &self.extensions.len())
            .finish()
    }
}

/// Extensions are opaque, equality compares only domain fields.
impl PartialEq for ModelInfo {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.provider == other.provider
            && self.display_name == other.display_name
            && self.aliases == other.aliases
            && self.caps == other.caps
            && self.pricing == other.pricing
    }
}

impl ModelInfo {
    /// Build a `ModelInfo` with permissive defaults. Useful for test
    /// stubs and ad-hoc providers that need a registered id without
    /// modelling real capability flags. Production providers should
    /// construct full `ModelInfo` values manually.
    #[must_use]
    pub fn minimal(id: impl Into<ModelId>, provider: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            provider: provider.into(),
            display_name: "(minimal)".into(),
            aliases: Vec::new(),
            caps: ModelCapabilities {
                max_input_tokens: 200_000,
                max_output_tokens: 4_096,
                default_max_output_tokens: 4_096,
                default_temperature_milli: 1_000,
                supports_tools: true,
                supports_vision: false,
                supports_audio: false,
                supports_thinking: false,
                supports_prompt_cache: false,
                supports_web_search: false,
                supports_temperature: true,
                reasoning_effort: None,
            },
            pricing: None,
            extensions: HashMap::new(),
        }
    }

    #[must_use]
    pub fn with_extension<T: ModelCapability>(mut self, ext: T) -> Self {
        self.extensions.insert(
            TypeId::of::<T>(),
            Arc::new(ext) as Arc<dyn Any + Send + Sync>,
        );
        self
    }

    #[must_use]
    pub fn capability<T: ModelCapability>(&self) -> Option<&T> {
        self.extensions
            .get(&TypeId::of::<T>())
            .and_then(|capability| capability.as_ref().downcast_ref::<T>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_caps() -> ModelCapabilities {
        ModelCapabilities {
            max_input_tokens: 200_000,
            max_output_tokens: 8_192,
            default_max_output_tokens: 4_096,
            default_temperature_milli: 1_000,
            supports_tools: true,
            supports_vision: true,
            supports_audio: false,
            supports_thinking: false,
            supports_prompt_cache: true,
            reasoning_effort: None,
            supports_web_search: false,
            supports_temperature: true,
        }
    }

    #[test]
    fn default_temperature_conversion_round_trips() {
        let caps = sample_caps();
        assert!((caps.default_temperature() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn default_temperature_zero_is_zero() {
        let caps = ModelCapabilities {
            default_temperature_milli: 0,
            ..sample_caps()
        };
        assert!(caps.default_temperature().abs() < 1e-6);
    }

    #[test]
    fn model_info_construction() {
        let info = ModelInfo {
            id: ModelId::new("claude-haiku-4-5"),
            provider: "anthropic".into(),
            display_name: "Claude Haiku 4.5".into(),
            aliases: vec![ModelId::new("claude-haiku-4-5-20251001")],
            caps: sample_caps(),
            pricing: Some(Pricing {
                input_per_mtok_usd: 1.0,
                output_per_mtok_usd: 5.0,
                cache_read_per_mtok_usd: None,
                cache_write_per_mtok_usd: None,
            }),
            extensions: HashMap::new(),
        };
        assert_eq!(info.id.as_str(), "claude-haiku-4-5");
        assert_eq!(info.provider, "anthropic");
        assert_eq!(info.aliases.len(), 1);
        assert!(info.pricing.is_some());
    }

    #[test]
    fn pricing_eq_uses_partial_eq() {
        let a = Pricing {
            input_per_mtok_usd: 1.0,
            output_per_mtok_usd: 5.0,
            cache_read_per_mtok_usd: None,
            cache_write_per_mtok_usd: None,
        };
        let b = Pricing {
            input_per_mtok_usd: 1.0,
            output_per_mtok_usd: 5.0,
            cache_read_per_mtok_usd: None,
            cache_write_per_mtok_usd: None,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn pricing_cache_fields_default_none() {
        let pricing = Pricing {
            input_per_mtok_usd: 1.0,
            output_per_mtok_usd: 5.0,
            cache_read_per_mtok_usd: None,
            cache_write_per_mtok_usd: None,
        };

        assert_eq!(pricing.cache_read_per_mtok_usd, None);
        assert_eq!(pricing.cache_write_per_mtok_usd, None);
    }

    struct TestCap {
        value: &'static str,
    }

    impl ModelCapability for TestCap {}

    struct OtherCap;

    impl ModelCapability for OtherCap {}

    #[test]
    fn model_info_with_extension_roundtrip() {
        let info = ModelInfo::minimal("test", "stub").with_extension(TestCap { value: "ok" });

        let cap = info.capability::<TestCap>().expect("capability exists");
        assert_eq!(cap.value, "ok");
    }

    #[test]
    fn capability_returns_none_for_missing() {
        let info = ModelInfo::minimal("test", "stub");

        assert!(info.capability::<TestCap>().is_none());
    }

    #[test]
    fn capability_returns_none_for_wrong_type() {
        let info = ModelInfo::minimal("test", "stub").with_extension(TestCap { value: "ok" });

        assert!(info.capability::<OtherCap>().is_none());
    }

    #[test]
    fn model_info_extension_ignored_in_partial_eq() {
        let left = ModelInfo::minimal("test", "stub").with_extension(TestCap { value: "left" });
        let right = ModelInfo::minimal("test", "stub").with_extension(OtherCap);

        assert_eq!(left, right);
    }

    #[test]
    fn minimal_sets_id_and_provider() {
        let info = ModelInfo::minimal("test", "stub");
        assert_eq!(info.id.as_str(), "test");
        assert_eq!(info.provider, "stub");
        assert!(info.aliases.is_empty());
        assert!(info.pricing.is_none());
    }

    #[test]
    fn minimal_caps_are_permissive() {
        let info = ModelInfo::minimal("test", "stub");
        assert!(info.caps.supports_tools);
        assert!(!info.caps.supports_vision);
        assert!(!info.caps.supports_audio);
        assert!(!info.caps.supports_thinking);
        assert!(!info.caps.supports_prompt_cache);
    }
}
