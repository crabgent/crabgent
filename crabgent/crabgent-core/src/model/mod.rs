//! Model-Registry: typed identifier + per-model capabilities.
//!
//! Three building blocks:
//!
//! - [`ModelId`]: newtype around `String`, used everywhere a model
//!   selection crosses a public boundary.
//! - [`ModelInfo`] (with [`ModelCapabilities`] and [`Pricing`]):
//!   metadata returned from a provider.
//! - [`ModelRegistry`]: collected by `KernelBuilder` from
//!   `Provider::models()`, consulted in the run-loop.

pub mod capability;
mod id;
mod info;
mod override_store;
mod registry;
mod selection;
#[cfg(test)]
mod selection_override_tests;
#[cfg(test)]
mod selection_tests;
mod target;

pub use capability::{ModelCapability, ReasoningEffort};
pub use id::ModelId;
pub use info::{ModelCapabilities, ModelInfo, Pricing};
pub use override_store::{
    EffortSource, GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore,
    ModelOverrideStoreError, NoopGlobalModelOverrideStore, NoopGlobalReasoningEffortOverrideStore,
    ReasoningEffortOverrideStoreError, ResolvedEffort, ResolvedModelWithSource, ResolvedSource,
};
pub use registry::{
    AmbiguousModelError, DuplicateModelError, ModelRegistry, ResolveModelError, UnknownModelError,
    UnknownModelTargetError,
};
pub(crate) use selection::{AttemptKind, request_for_attempt};
pub use selection::{
    EffectiveParams, ResolvedModel, effective_params, map_resolve_error, resolve_attempts,
    resolve_model, resolve_model_target, resolve_model_target_with_overrides,
    resolve_model_with_overrides, validate_model_override,
};
pub use target::ModelTarget;
