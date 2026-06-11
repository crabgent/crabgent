//! Model selection lifecycle and capability-driven request defaults.

use crate::error::KernelError;
use crate::model::{
    GlobalModelOverrideStore, ModelId, ModelInfo, ModelRegistry, ModelTarget, ReasoningEffort,
    ResolveModelError, ResolvedModelWithSource, ResolvedSource,
};
use crate::types::LlmRequest;
/// Capability-resolved request parameters: pin `max_tokens` and
/// `temperature` to the model's defaults when the caller did not
/// specify them.
pub struct EffectiveParams {
    pub max_tokens: u32,
    pub temperature: f32,
}

/// Provider-qualified model plus its canonical registry info.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub target: ModelTarget,
    pub info: ModelInfo,
}

/// Position of a resolved model attempt inside one fallback chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptKind {
    Primary,
    Fallback,
}

/// Apply per-model defaults from `info.caps` for every parameter the
/// caller left as `None`.
pub fn effective_params(
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    info: &ModelInfo,
) -> EffectiveParams {
    EffectiveParams {
        max_tokens: max_tokens.unwrap_or(info.caps.default_max_output_tokens),
        temperature: temperature.unwrap_or_else(|| info.caps.default_temperature()),
    }
}

/// Strict registry lookup for the primary request target. Unqualified
/// ids must resolve to a single provider; provider-qualified targets
/// pin the request to that provider.
pub fn resolve_model(
    registry: &ModelRegistry,
    model: &ModelTarget,
) -> Result<ResolvedModel, KernelError> {
    match model {
        ModelTarget::Id(id) => {
            let info = registry.require(id).cloned().map_err(map_resolve_error)?;
            Ok(ResolvedModel {
                target: ModelTarget::new(info.provider.clone(), id.clone()),
                info,
            })
        }
        ModelTarget::Provider { .. } => resolve_model_target(registry, model),
    }
}

/// Strict provider-qualified lookup for fallback targets.
pub fn resolve_model_target(
    registry: &ModelRegistry,
    target: &ModelTarget,
) -> Result<ResolvedModel, KernelError> {
    let info = registry
        .resolve(target)
        .cloned()
        .map_err(map_resolve_error)?;
    Ok(ResolvedModel {
        target: ModelTarget::new(info.provider.clone(), target.model().clone()),
        info,
    })
}

pub fn map_resolve_error(err: ResolveModelError) -> KernelError {
    match err {
        ResolveModelError::Unknown(err) => KernelError::UnknownModel(err.0),
        ResolveModelError::Ambiguous(err) => KernelError::AmbiguousModel(err.0),
        ResolveModelError::UnknownTarget(err) => KernelError::UnknownModelTarget(err.0),
        ResolveModelError::UnknownOverride { scope, model } => {
            KernelError::UnknownModelOverride { scope, model }
        }
        ResolveModelError::OverrideStore(err) => KernelError::ModelOverrideStore {
            reason: err.to_string(),
        },
    }
}

/// Resolve the effective model id from explicit, session, global, and default
/// layers. Overrides must point at registered, unambiguous model ids.
pub async fn resolve_model_with_overrides(
    registry: &ModelRegistry,
    session_override: Option<&ModelId>,
    global_store: &dyn GlobalModelOverrideStore,
    explicit: Option<&ModelId>,
    default: &ModelId,
) -> Result<ResolvedModelWithSource, ResolveModelError> {
    if let Some(model) = explicit {
        return resolve_id_with_source(registry, model, ResolvedSource::Explicit);
    }
    if let Some(model) = session_override {
        return resolve_override(registry, "session", model, ResolvedSource::SessionOverride);
    }
    if let Some(model) = global_store.get_global_model_override().await? {
        return resolve_override(registry, "global", &model, ResolvedSource::GlobalOverride);
    }
    resolve_id_with_source(registry, default, ResolvedSource::ConfigDefault)
}

/// Resolve the effective model target while preserving provider-qualified
/// explicit/default targets. Session and global overrides remain unqualified
/// `ModelId`s because those values are persisted by users.
pub async fn resolve_model_target_with_overrides(
    registry: &ModelRegistry,
    session_override: Option<&ModelId>,
    global_store: &dyn GlobalModelOverrideStore,
    explicit: Option<&ModelTarget>,
    default: &ModelTarget,
) -> Result<ResolvedModelWithSource, ResolveModelError> {
    if let ModelTarget::Id(default_id) = default
        && explicit
            .as_ref()
            .is_none_or(|target| matches!(target, ModelTarget::Id(_)))
    {
        let explicit_id = explicit.map(ModelTarget::model);
        return resolve_model_with_overrides(
            registry,
            session_override,
            global_store,
            explicit_id,
            default_id,
        )
        .await;
    }
    if let Some(target) = explicit {
        return resolve_target_with_source(registry, target, ResolvedSource::Explicit);
    }
    if let Some(model) = session_override {
        return resolve_override(registry, "session", model, ResolvedSource::SessionOverride);
    }
    if let Some(model) = global_store.get_global_model_override().await? {
        return resolve_override(registry, "global", &model, ResolvedSource::GlobalOverride);
    }
    resolve_target_with_source(registry, default, ResolvedSource::ConfigDefault)
}

fn resolve_id_with_source(
    registry: &ModelRegistry,
    model: &ModelId,
    source: ResolvedSource,
) -> Result<ResolvedModelWithSource, ResolveModelError> {
    let info = registry.require(model)?.clone();
    Ok(ResolvedModelWithSource { info, source })
}

fn resolve_target_with_source(
    registry: &ModelRegistry,
    target: &ModelTarget,
    source: ResolvedSource,
) -> Result<ResolvedModelWithSource, ResolveModelError> {
    let info = registry.resolve(target)?.clone();
    Ok(ResolvedModelWithSource { info, source })
}

fn resolve_override(
    registry: &ModelRegistry,
    scope: &'static str,
    model: &ModelId,
    source: ResolvedSource,
) -> Result<ResolvedModelWithSource, ResolveModelError> {
    let Some(info) = registry.get(model).cloned() else {
        return Err(ResolveModelError::UnknownOverride {
            scope,
            model: model.clone(),
        });
    };
    Ok(ResolvedModelWithSource { info, source })
}

/// Validate a persisted model override against the registry without
/// selecting it as the current model.
pub fn validate_model_override(
    registry: &ModelRegistry,
    scope: &'static str,
    model: &ModelId,
) -> Result<(), ResolveModelError> {
    resolve_override(registry, scope, model, ResolvedSource::Explicit).map(|_| ())
}

/// Resolve the primary request model followed by provider-qualified fallback
/// targets.
///
/// The primary must resolve: an unknown primary model is the run's real, fatal
/// error and fails closed. Fallbacks are best-effort degradation, so a fallback
/// target the registry does not know (for example a model deactivated after the
/// operator configured it) is dropped from the chain instead of aborting a run
/// whose primary would have succeeded. The core is log-free; operators who want
/// deploy-time feedback on a stale fallback list validate it against
/// [`crate::Kernel::models`] via [`ModelRegistry::require_target`].
pub fn resolve_attempts(
    registry: &ModelRegistry,
    primary: &ModelTarget,
    fallbacks: &[ModelTarget],
) -> Result<Vec<ResolvedModel>, KernelError> {
    let mut attempts = Vec::with_capacity(1 + fallbacks.len());
    attempts.push(resolve_model(registry, primary)?);
    attempts.extend(
        fallbacks
            .iter()
            .filter_map(|target| resolve_model_target(registry, target).ok()),
    );
    Ok(attempts)
}

/// Clone a hook-mutated request for one concrete provider/model attempt.
///
/// This is the single source of truth for per-attempt request shape: callers
/// must not transform the request after this point. It selects the model,
/// applies model defaults, and downgrades fallback-only web-search and
/// reasoning-effort features before pre-flight. Tools and vision remain
/// terminal on unsupported fallbacks until consumer-side needs downgrade
/// semantics for them.
pub fn request_for_attempt(
    base: &LlmRequest,
    attempt: &ResolvedModel,
    attempt_kind: AttemptKind,
) -> LlmRequest {
    let params = effective_params(base.max_tokens, base.temperature, &attempt.info);
    let mut req = base.clone();
    req.model = attempt.target.model().clone();
    req.max_tokens = Some(params.max_tokens);
    req.temperature = if attempt.info.caps.supports_temperature {
        Some(params.temperature)
    } else {
        // Anthropic opus-4-7 and any other model registered with
        // `supports_temperature: false` returns HTTP 400 on any
        // temperature value. Omit the field so the wire body skips
        // the JSON key entirely.
        None
    };
    if (req.reasoning_effort == Some(ReasoningEffort::Disabled)
        || matches!(attempt_kind, AttemptKind::Fallback))
        && attempt.info.caps.reasoning_effort.is_none()
    {
        req.reasoning_effort = None;
    } else {
        req.reasoning_effort = req.reasoning_effort.or(attempt.info.caps.reasoning_effort);
    }
    if matches!(attempt_kind, AttemptKind::Fallback) && !attempt.info.caps.supports_web_search {
        req.web_search = crate::types::WebSearchConfig::default();
    }
    req
}
