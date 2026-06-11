//! Provider collection plus model-catalog assembly.

use std::collections::HashSet;
use std::sync::Arc;

use crate::error::KernelError;
use crate::model::{ModelId, ModelInfo, ModelRegistry};
use crate::provider::Provider;
use thiserror::Error;

/// Errors returned when a configured provider catalog cannot form a
/// deterministic kernel registry.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum BuildError {
    #[error("duplicate provider name: {provider_name}")]
    DuplicateProvider { provider_name: String },
    #[error("provider `{provider}` has no advertised models")]
    EmptyModelList { provider: String },
    #[error("duplicate model id: {model_id}")]
    DuplicateModel { model_id: ModelId },
    #[error("provider {provider} returned mismatched model {model}")]
    ProviderMismatch { provider: String, model: ModelId },
}

/// Providers registered on a [`Kernel`](crate::Kernel).
pub struct ProviderSet {
    providers: Vec<Arc<dyn Provider>>,
}

impl ProviderSet {
    pub fn try_new(providers: Vec<Arc<dyn Provider>>) -> Result<Self, BuildError> {
        debug_assert!(!providers.is_empty(), "typestate requires one provider");
        ensure_unique_provider_names(&providers)?;
        Ok(Self { providers })
    }

    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "ProviderSet::try_new is only constructed from non-empty typestate builder input"
    )]
    pub fn primary(&self) -> &Arc<dyn Provider> {
        self.providers
            .first()
            .expect("typestate invariant: provider set is non-empty")
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    #[must_use]
    pub fn all(&self) -> &[Arc<dyn Provider>] {
        &self.providers
    }

    pub fn provider_named(&self, name: &str) -> Result<&Arc<dyn Provider>, KernelError> {
        self.providers
            .iter()
            .find(|provider| provider.name() == name)
            .ok_or_else(|| KernelError::Internal(format!("unregistered provider: {name}")))
    }

    pub fn try_build_model_registry(&self) -> Result<ModelRegistry, BuildError> {
        let mut registry = ModelRegistry::new();
        for provider in &self.providers {
            register_provider_models(provider.as_ref(), &mut registry)?;
        }
        Ok(registry)
    }
}

fn ensure_unique_provider_names(providers: &[Arc<dyn Provider>]) -> Result<(), BuildError> {
    let mut seen = HashSet::with_capacity(providers.len());
    for provider in providers {
        let name = provider.name();
        if !seen.insert(name) {
            return Err(BuildError::DuplicateProvider {
                provider_name: name.into(),
            });
        }
    }
    Ok(())
}

fn ensure_model_provider_matches(
    provider: &dyn Provider,
    info: &ModelInfo,
) -> Result<(), BuildError> {
    if info.provider == provider.name() {
        Ok(())
    } else {
        Err(BuildError::ProviderMismatch {
            provider: provider.name().into(),
            model: info.id.clone(),
        })
    }
}

fn register_provider_models(
    provider: &dyn Provider,
    registry: &mut ModelRegistry,
) -> Result<(), BuildError> {
    let entries = provider.models();
    if entries.is_empty() {
        return Err(BuildError::EmptyModelList {
            provider: provider.name().into(),
        });
    }
    for info in entries {
        ensure_model_provider_matches(provider, &info)?;
        registry
            .insert(info)
            .map_err(|e| BuildError::DuplicateModel {
                model_id: e.0.model().clone(),
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{BuildError, ProviderSet};
    use crate::error::ProviderError;
    use crate::hook::RunCtx;
    use crate::model::{ModelId, ModelInfo};
    use crate::provider::Provider;
    use crate::types::{LlmRequest, LlmResponse};
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct EmptyModelsProvider;

    #[async_trait]
    impl Provider for EmptyModelsProvider {
        async fn complete(
            &self,
            _req: &LlmRequest,
            _ctx: &RunCtx,
            _cancel: Option<&CancellationToken>,
        ) -> Result<LlmResponse, ProviderError> {
            Err(ProviderError::Other(
                "should not be called in model registration tests".into(),
            ))
        }

        fn name(&self) -> &'static str {
            "empty-models"
        }

        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            crate::provider::ProviderCapabilities::default()
        }

        fn models(&self) -> Vec<ModelInfo> {
            vec![]
        }
    }

    struct ProviderWithModel;

    #[async_trait]
    impl Provider for ProviderWithModel {
        async fn complete(
            &self,
            _req: &LlmRequest,
            _ctx: &RunCtx,
            _cancel: Option<&CancellationToken>,
        ) -> Result<LlmResponse, ProviderError> {
            Err(ProviderError::Other(
                "should not be called in model registration tests".into(),
            ))
        }

        fn name(&self) -> &'static str {
            "stub"
        }

        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            crate::provider::ProviderCapabilities::default()
        }

        fn models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo::minimal("stub", "stub")]
        }
    }

    #[test]
    fn register_provider_models_rejects_empty_provider_models() {
        let provider_set =
            ProviderSet::try_new(vec![Arc::new(EmptyModelsProvider)]).expect("provider set ok");

        let err = provider_set
            .try_build_model_registry()
            .expect_err("empty model list rejected");

        assert!(matches!(
            err,
            BuildError::EmptyModelList {
                provider: ref name,
            } if name == "empty-models"
        ));
    }

    #[test]
    fn register_provider_models_accepts_valid_provider_models() {
        let provider_set =
            ProviderSet::try_new(vec![Arc::new(ProviderWithModel)]).expect("provider set ok");
        let registry = provider_set
            .try_build_model_registry()
            .expect("non-empty model list accepted");

        assert!(registry.get(&ModelId::new("stub")).is_some());
    }

    #[test]
    fn provider_set_compares_string_provider_correctly() {
        let provider = ProviderWithModel;
        let info = ModelInfo::minimal("stub", String::from(provider.name()));

        super::ensure_model_provider_matches(&provider, &info)
            .expect("String provider equals provider name");
    }
}
