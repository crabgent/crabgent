//! `ModelRegistry`: lookup of registered `ModelInfo` by provider/model target,
//! id, and alias.
//!
//! Built by `KernelBuilder` from `Provider::models()`. Run-loop
//! consults the registry before every `Provider::complete()` call to
//! reject unknown identifiers fail-closed.

use std::collections::{HashMap, HashSet};

use thiserror::Error;

use crate::model::id::ModelId;
use crate::model::info::ModelInfo;
use crate::model::override_store::ModelOverrideStoreError;
use crate::model::target::ModelTarget;

/// Failure when registering a model whose id collides with one already
/// registered (canonical id or alias).
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("model id collision: {0}")]
pub struct DuplicateModelError(pub ModelTarget);

/// Failure when looking up an id that the registry does not know.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("unknown model: {0}")]
pub struct UnknownModelError(pub ModelId);

/// Failure when an unqualified model id maps to multiple providers.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("ambiguous model: {0}")]
pub struct AmbiguousModelError(pub ModelId);

/// Failure when looking up a provider/model target that the registry does
/// not know.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("unknown model target: {0}")]
pub struct UnknownModelTargetError(pub ModelTarget);

/// Failure when resolving a [`ModelTarget`] selector.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolveModelError {
    #[error(transparent)]
    Unknown(#[from] UnknownModelError),
    #[error(transparent)]
    Ambiguous(#[from] AmbiguousModelError),
    #[error(transparent)]
    UnknownTarget(#[from] UnknownModelTargetError),
    /// A persisted override references a model that is no longer registered.
    #[error("unknown {scope} model override: {model}")]
    UnknownOverride { scope: &'static str, model: ModelId },
    /// Reading a persisted override failed.
    #[error(transparent)]
    OverrideStore(#[from] ModelOverrideStoreError),
}

/// In-memory registry of `ModelInfo`, keyed by provider-qualified canonical
/// target with a secondary map of provider-qualified aliases pointing to
/// canonical targets.
#[derive(Debug, Default)]
pub struct ModelRegistry {
    canonical: HashMap<ModelTarget, ModelInfo>,
    aliases: HashMap<ModelTarget, ModelTarget>,
    unqualified: HashMap<ModelId, ModelTarget>,
    ambiguous: HashSet<ModelId>,
}

impl ModelRegistry {
    /// Build an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a model. Fails if either the canonical id or any alias
    /// is already registered for the same provider.
    pub fn insert(&mut self, info: ModelInfo) -> Result<(), DuplicateModelError> {
        let canonical = ModelTarget::new(info.provider.clone(), info.id.clone());
        if self.is_target_taken(&canonical) {
            return Err(DuplicateModelError(canonical));
        }
        let mut local = HashSet::from([canonical.clone()]);
        for a in &info.aliases {
            let alias = ModelTarget::new(info.provider.clone(), a.clone());
            if self.is_target_taken(&alias) || !local.insert(alias.clone()) {
                return Err(DuplicateModelError(alias));
            }
        }
        let id = info.id.clone();
        let aliases = info.aliases.clone();
        for a in &aliases {
            self.aliases.insert(
                ModelTarget::new(info.provider.clone(), a.clone()),
                canonical.clone(),
            );
        }
        self.index_unqualified(&id, &canonical);
        for a in &aliases {
            self.index_unqualified(a, &canonical);
        }
        self.canonical.insert(canonical, info);
        Ok(())
    }

    fn is_target_taken(&self, target: &ModelTarget) -> bool {
        self.canonical.contains_key(target) || self.aliases.contains_key(target)
    }

    fn index_unqualified(&mut self, id: &ModelId, target: &ModelTarget) {
        if self.ambiguous.contains(id) {
            return;
        }
        match self.unqualified.get(id) {
            Some(existing) if existing != target => {
                self.unqualified.remove(id);
                self.ambiguous.insert(id.clone());
            }
            Some(_) => {}
            None => {
                self.unqualified.insert(id.clone(), target.clone());
            }
        }
    }

    /// Look up a `ModelInfo` by canonical id or alias. Returns
    /// `None` for unknown or ambiguous ids.
    #[must_use]
    pub fn get(&self, id: &ModelId) -> Option<&ModelInfo> {
        if self.ambiguous.contains(id) {
            return None;
        }
        let target = self.unqualified.get(id)?;
        self.get_target(target)
    }

    /// Look up a `ModelInfo` by provider-qualified canonical id or alias.
    /// Returns `None` for unknown targets.
    #[must_use]
    pub fn get_target(&self, target: &ModelTarget) -> Option<&ModelInfo> {
        match target {
            ModelTarget::Id(id) => self.get(id),
            ModelTarget::Provider { .. } => {
                let canonical = self.aliases.get(target).unwrap_or(target);
                self.canonical.get(canonical)
            }
        }
    }

    /// Strict lookup for either an unqualified id or provider-qualified target.
    pub fn resolve(&self, target: &ModelTarget) -> Result<&ModelInfo, ResolveModelError> {
        match target {
            ModelTarget::Id(id) => self.require(id),
            ModelTarget::Provider { .. } => self
                .require_target(target)
                .map_err(ResolveModelError::UnknownTarget),
        }
    }

    /// Strict variant of [`Self::get`] used by the run-loop.
    pub fn require(&self, id: &ModelId) -> Result<&ModelInfo, ResolveModelError> {
        if self.ambiguous.contains(id) {
            return Err(ResolveModelError::Ambiguous(AmbiguousModelError(
                id.clone(),
            )));
        }
        self.get(id)
            .ok_or_else(|| ResolveModelError::Unknown(UnknownModelError(id.clone())))
    }

    /// Strict variant of [`Self::get_target`] used by fallback routing.
    pub fn require_target(
        &self,
        target: &ModelTarget,
    ) -> Result<&ModelInfo, UnknownModelTargetError> {
        self.get_target(target)
            .ok_or_else(|| UnknownModelTargetError(target.clone()))
    }

    /// Iterator over all registered canonical infos (alias entries
    /// are deduplicated).
    pub fn list(&self) -> impl Iterator<Item = &ModelInfo> + '_ {
        self.canonical.values()
    }

    /// Number of canonical models registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.canonical.len()
    }

    /// `true` if no models are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.canonical.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::info::ModelCapabilities;

    fn sample_caps() -> ModelCapabilities {
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
            reasoning_effort: None,
            supports_web_search: false,
            supports_temperature: true,
        }
    }

    fn info_with_provider(provider: impl Into<String>, id: &str, aliases: &[&str]) -> ModelInfo {
        ModelInfo {
            id: ModelId::new(id),
            provider: provider.into(),
            display_name: "Stub".into(),
            aliases: aliases.iter().map(|a| ModelId::new(*a)).collect(),
            caps: sample_caps(),
            pricing: None,
            extensions: HashMap::new(),
        }
    }

    fn info_with(id: &str, aliases: &[&str]) -> ModelInfo {
        info_with_provider("stub", id, aliases)
    }

    #[test]
    fn empty_registry_reports_empty() {
        let r = ModelRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn insert_and_get_canonical() {
        let mut r = ModelRegistry::new();
        r.insert(info_with("haiku", &[])).expect("test result");
        assert_eq!(r.len(), 1);
        assert!(r.get(&ModelId::new("haiku")).is_some());
    }

    #[test]
    fn insert_with_alias_resolves_to_canonical() {
        let mut r = ModelRegistry::new();
        r.insert(info_with("haiku", &["haiku-latest", "haiku-2026"]))
            .expect("test result");
        let a = r.get(&ModelId::new("haiku-latest")).expect("test result");
        let b = r.get(&ModelId::new("haiku-2026")).expect("test result");
        assert_eq!(a.id, b.id);
        assert_eq!(a.id.as_str(), "haiku");
    }

    #[test]
    fn insert_duplicate_canonical_fails() {
        let mut r = ModelRegistry::new();
        r.insert(info_with("haiku", &[])).expect("test result");
        let err = r
            .insert(info_with("haiku", &[]))
            .expect_err("expected error");
        assert_eq!(err.0.provider(), Some("stub"));
        assert_eq!(err.0.as_str(), "haiku");
    }

    #[test]
    fn insert_alias_collides_with_existing_canonical() {
        let mut r = ModelRegistry::new();
        r.insert(info_with("haiku", &[])).expect("test result");
        let err = r
            .insert(info_with("sonnet", &["haiku"]))
            .expect_err("expected error");
        assert_eq!(err.0.provider(), Some("stub"));
        assert_eq!(err.0.as_str(), "haiku");
    }

    #[test]
    fn insert_alias_collides_with_existing_alias() {
        let mut r = ModelRegistry::new();
        r.insert(info_with("haiku", &["latest"]))
            .expect("test result");
        let err = r
            .insert(info_with("sonnet", &["latest"]))
            .expect_err("expected error");
        assert_eq!(err.0.provider(), Some("stub"));
        assert_eq!(err.0.as_str(), "latest");
    }

    #[test]
    fn duplicate_model_ids_are_allowed_across_providers() {
        let mut r = ModelRegistry::new();
        r.insert(info_with_provider("anthropic", "opus", &[]))
            .expect("test result");
        r.insert(info_with_provider("openai", "opus", &[]))
            .expect("test result");

        assert_eq!(r.len(), 2);
        assert!(r.get(&ModelId::new("opus")).is_none());
        assert_eq!(
            r.get_target(&ModelTarget::new("anthropic", "opus"))
                .expect("test result")
                .provider,
            "anthropic"
        );
        assert_eq!(
            r.get_target(&ModelTarget::new("openai", "opus"))
                .expect("test result")
                .provider,
            "openai"
        );
    }

    #[test]
    fn target_lookup_resolves_provider_alias() {
        let mut r = ModelRegistry::new();
        r.insert(info_with_provider("anthropic", "opus", &["opus-latest"]))
            .expect("test result");

        let info = r
            .require_target(&ModelTarget::new("anthropic", "opus-latest"))
            .expect("test result");

        assert_eq!(info.id.as_str(), "opus");
        assert_eq!(info.provider, "anthropic");
    }

    #[test]
    fn require_target_err_on_unknown_target() {
        let r = ModelRegistry::new();
        let target = ModelTarget::new("missing", "model");
        let err = r.require_target(&target).expect_err("expected error");
        assert_eq!(err.0, target);
    }

    #[test]
    fn require_ok_on_known_id() {
        let mut r = ModelRegistry::new();
        r.insert(info_with("haiku", &[])).expect("test result");
        let info = r.require(&ModelId::new("haiku")).expect("test result");
        assert_eq!(info.id.as_str(), "haiku");
    }

    #[test]
    fn require_err_on_unknown_id() {
        let r = ModelRegistry::new();
        let err = r
            .require(&ModelId::new("missing"))
            .expect_err("expected error");
        match err {
            ResolveModelError::Unknown(err) => assert_eq!(err.0.as_str(), "missing"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn ambiguous_model_error_distinct_from_unknown() {
        let mut r = ModelRegistry::new();
        r.insert(info_with_provider("anthropic", "opus", &[]))
            .expect("test result");
        r.insert(info_with_provider("openai", "opus", &[]))
            .expect("test result");

        let err = r
            .require(&ModelId::new("opus"))
            .expect_err("expected error");

        match err {
            ResolveModelError::Ambiguous(err) => assert_eq!(err.0.as_str(), "opus"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn model_target_provider_qualified_resolves() {
        let mut r = ModelRegistry::new();
        r.insert(info_with_provider("anthropic", "opus", &[]))
            .expect("test result");
        r.insert(info_with_provider("openai", "gpt", &[]))
            .expect("test result");

        let info = r
            .resolve(&ModelTarget::new("anthropic", "opus"))
            .expect("test result");

        assert_eq!(info.provider, "anthropic");
        assert_eq!(info.id.as_str(), "opus");
    }

    #[test]
    fn list_returns_one_per_canonical() {
        let mut r = ModelRegistry::new();
        r.insert(info_with("haiku", &["haiku-latest"]))
            .expect("test result");
        r.insert(info_with("sonnet", &[])).expect("test result");
        let names: Vec<_> = r.list().map(|i| i.id.as_str().to_owned()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"haiku".to_owned()));
        assert!(names.contains(&"sonnet".to_owned()));
    }

    #[test]
    fn get_unknown_returns_none() {
        let r = ModelRegistry::new();
        assert!(r.get(&ModelId::new("nope")).is_none());
    }
}
