//! Output serialization for the model registry tool.

use crabgent_core::model::{
    ModelId, ModelInfo, ReasoningEffort, ResolvedEffort, ResolvedModelWithSource,
};
use serde_json::{Map, Value, json};

pub const MAX_LIST_MODELS: usize = 200;

pub fn model_info_to_json(info: &ModelInfo) -> Value {
    let aliases: Vec<&str> = info.aliases.iter().map(ModelId::as_str).collect();
    let mut model = json!({
        "id": info.id.as_str(),
        "provider": info.provider.as_str(),
        "display_name": info.display_name.as_str(),
        "aliases": aliases,
        "caps": {
            "max_input_tokens": info.caps.max_input_tokens,
            "max_output_tokens": info.caps.max_output_tokens,
            "supports_tools": info.caps.supports_tools,
            "supports_vision": info.caps.supports_vision,
            "supports_audio": info.caps.supports_audio,
            "supports_thinking": info.caps.supports_thinking,
            "supports_prompt_cache": info.caps.supports_prompt_cache,
            "supports_web_search": info.caps.supports_web_search,
            "reasoning_effort": info.caps.reasoning_effort.map(ReasoningEffort::as_str),
        },
    });
    if let Some(pricing) = &info.pricing {
        let mut pricing_json = Map::new();
        pricing_json.insert(
            "input_per_mtok_usd".to_owned(),
            json!(pricing.input_per_mtok_usd),
        );
        pricing_json.insert(
            "output_per_mtok_usd".to_owned(),
            json!(pricing.output_per_mtok_usd),
        );
        if let Some(cache_read) = pricing.cache_read_per_mtok_usd {
            pricing_json.insert("cache_read_per_mtok_usd".to_owned(), json!(cache_read));
        }
        if let Some(cache_write) = pricing.cache_write_per_mtok_usd {
            pricing_json.insert("cache_write_per_mtok_usd".to_owned(), json!(cache_write));
        }
        if let Some(model) = model.as_object_mut() {
            model.insert("pricing".to_owned(), Value::Object(pricing_json));
        }
    }
    model
}

pub fn current_model_to_json(
    current: &ResolvedModelWithSource,
    current_effort: ResolvedEffort,
    session_override: Option<&str>,
    global_override: Option<&ModelId>,
    session_effort_override: Option<ReasoningEffort>,
    global_effort_override: Option<ReasoningEffort>,
) -> Value {
    json!({
        "model": model_info_to_json(&current.info),
        "source": current.source.as_str(),
        "override_session": session_override,
        "override_global": global_override.map(ModelId::as_str),
        "reasoning_effort": current_effort.effort.map(ReasoningEffort::as_str),
        "reasoning_effort_source": current_effort.source.as_str(),
        "override_session_effort": session_effort_override.map(ReasoningEffort::as_str),
        "override_global_effort": global_effort_override.map(ReasoningEffort::as_str),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crabgent_core::model::{ModelCapabilities, Pricing};

    use super::*;

    fn caps() -> ModelCapabilities {
        ModelCapabilities {
            max_input_tokens: 200_000,
            max_output_tokens: 8_192,
            default_max_output_tokens: 4_096,
            default_temperature_milli: 1_000,
            supports_tools: true,
            supports_vision: true,
            supports_audio: false,
            supports_thinking: true,
            supports_prompt_cache: true,
            supports_web_search: false,
            supports_temperature: true,
            reasoning_effort: None,
        }
    }

    fn info(pricing: Option<Pricing>) -> ModelInfo {
        ModelInfo {
            id: ModelId::new("claude-sonnet-4-6"),
            provider: "anthropic".to_owned(),
            display_name: "Claude Sonnet 4.6".to_owned(),
            aliases: vec![ModelId::new("sonnet")],
            caps: caps(),
            pricing,
            extensions: HashMap::new(),
        }
    }

    #[test]
    fn model_info_to_json_includes_all_capabilities() {
        let json = model_info_to_json(&info(None));

        assert_eq!(json["caps"]["max_input_tokens"], 200_000);
        assert_eq!(json["caps"]["max_output_tokens"], 8_192);
        assert_eq!(json["caps"]["supports_tools"], true);
        assert_eq!(json["caps"]["supports_vision"], true);
        assert_eq!(json["caps"]["supports_audio"], false);
        assert_eq!(json["caps"]["supports_thinking"], true);
        assert_eq!(json["caps"]["supports_prompt_cache"], true);
        assert_eq!(json["caps"]["supports_web_search"], false);
        assert!(json["caps"].get("default_max_output_tokens").is_none());
        assert!(json["caps"].get("default_temperature_milli").is_none());
    }

    #[test]
    fn model_info_to_json_includes_pricing_when_some() {
        let json = model_info_to_json(&info(Some(Pricing {
            input_per_mtok_usd: 3.0,
            output_per_mtok_usd: 15.0,
            cache_read_per_mtok_usd: Some(0.3),
            cache_write_per_mtok_usd: Some(3.75),
        })));

        assert_eq!(json["pricing"]["input_per_mtok_usd"], 3.0);
        assert_eq!(json["pricing"]["output_per_mtok_usd"], 15.0);
        assert_eq!(json["pricing"]["cache_read_per_mtok_usd"], 0.3);
        assert_eq!(json["pricing"]["cache_write_per_mtok_usd"], 3.75);
    }

    #[test]
    fn model_info_to_json_omits_pricing_when_none() {
        let json = model_info_to_json(&info(None));

        assert!(json.get("pricing").is_none());
    }

    #[test]
    fn model_info_to_json_omits_extensions_key() {
        let json = model_info_to_json(&info(None));

        assert!(json.get("extensions").is_none());
    }
}
