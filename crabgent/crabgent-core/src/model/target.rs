//! Model selection for run requests and fallback chains.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::model::id::ModelId;

/// Model selector. `Id` lets the registry choose the provider when the id is
/// unique. `Provider` pins the request to a concrete provider/model pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelTarget {
    Id(ModelId),
    Provider { provider: String, model: ModelId },
}

impl ModelTarget {
    #[must_use]
    pub fn new(provider: impl Into<String>, model: impl Into<ModelId>) -> Self {
        Self::Provider {
            provider: provider.into().trim().to_owned(),
            model: model.into(),
        }
    }

    #[must_use]
    pub fn id(model: impl Into<ModelId>) -> Self {
        Self::Id(model.into())
    }

    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        match self {
            Self::Id(_) => None,
            Self::Provider { provider, .. } => Some(provider),
        }
    }

    #[must_use]
    pub const fn model(&self) -> &ModelId {
        match self {
            Self::Id(model) | Self::Provider { model, .. } => model,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.model().as_str()
    }
}

impl From<ModelId> for ModelTarget {
    fn from(model: ModelId) -> Self {
        Self::id(model)
    }
}

impl From<&str> for ModelTarget {
    fn from(model: &str) -> Self {
        Self::id(model)
    }
}

impl From<String> for ModelTarget {
    fn from(model: String) -> Self {
        Self::id(model)
    }
}

impl fmt::Display for ModelTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(model) => model.fmt(f),
            Self::Provider { provider, model } => write!(f, "{provider}/{model}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_trims_provider_and_model() {
        let t = ModelTarget::new(" anthropic ", " opus ");
        assert_eq!(t.provider(), Some("anthropic"));
        assert_eq!(t.as_str(), "opus");
    }

    #[test]
    fn display_uses_provider_slash_model() {
        let t = ModelTarget::new("openai", "gpt-5.5");
        assert_eq!(t.to_string(), "openai/gpt-5.5");
    }

    #[test]
    fn id_target_displays_model_id_only() {
        let t = ModelTarget::id("gpt-5.5");
        assert_eq!(t.provider(), None);
        assert_eq!(t.to_string(), "gpt-5.5");
    }
}
