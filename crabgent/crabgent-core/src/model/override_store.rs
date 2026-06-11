//! Model override persistence surface shared by the kernel and store backends.

use async_trait::async_trait;
use thiserror::Error;

use crate::model::{ModelId, ModelInfo, ReasoningEffort};

/// Failure returned by a model-override store.
///
/// This lives in `crabgent-core` so the kernel can depend on the trait without
/// depending on `crabgent-store`. Concrete store crates map their backend errors
/// into this opaque shape.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelOverrideStoreError {
    /// Caller supplied an invalid opaque session id.
    #[error("invalid session id: {0}")]
    InvalidSessionId(String),
    /// Requested override target does not exist.
    #[error("{kind} not found: {id}")]
    NotFound { kind: &'static str, id: String },
    /// Backend-specific failure, with secrets and connection strings already
    /// stripped by the backend implementation.
    #[error("model override store backend error: {0}")]
    Backend(String),
}

impl ModelOverrideStoreError {
    /// Construct an opaque backend error from a displayable value.
    pub fn backend(value: impl std::fmt::Display) -> Self {
        Self::Backend(value.to_string())
    }
}

/// Store for the global model override.
#[async_trait]
pub trait GlobalModelOverrideStore: Send + Sync + 'static {
    /// Return the configured global model override, if present.
    async fn get_global_model_override(&self) -> Result<Option<ModelId>, ModelOverrideStoreError>;

    /// Replace the global model override.
    async fn set_global_model_override(
        &self,
        model: &ModelId,
    ) -> Result<(), ModelOverrideStoreError>;

    /// Clear the global model override.
    async fn clear_global_model_override(&self) -> Result<(), ModelOverrideStoreError>;
}

/// Failure returned by a reasoning-effort override store.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReasoningEffortOverrideStoreError {
    /// Backend-specific failure, with secrets and connection strings already
    /// stripped by the backend implementation.
    #[error("reasoning effort override store backend error: {0}")]
    Backend(String),
}

impl ReasoningEffortOverrideStoreError {
    /// Construct an opaque backend error from a displayable value.
    pub fn backend(value: impl std::fmt::Display) -> Self {
        Self::Backend(value.to_string())
    }
}

/// Store for the global reasoning-effort override.
#[async_trait]
pub trait GlobalReasoningEffortOverrideStore: Send + Sync + 'static {
    /// Return the configured global reasoning-effort override, if present.
    async fn get_global_reasoning_effort_override(
        &self,
    ) -> Result<Option<ReasoningEffort>, ReasoningEffortOverrideStoreError>;

    /// Replace the global reasoning-effort override.
    async fn set_global_reasoning_effort_override(
        &self,
        effort: ReasoningEffort,
    ) -> Result<(), ReasoningEffortOverrideStoreError>;

    /// Clear the global reasoning-effort override.
    async fn clear_global_reasoning_effort_override(
        &self,
    ) -> Result<(), ReasoningEffortOverrideStoreError>;
}

/// Default kernel override store: no override and no persistence.
#[derive(Debug, Default)]
pub struct NoopGlobalModelOverrideStore;

#[async_trait]
impl GlobalModelOverrideStore for NoopGlobalModelOverrideStore {
    async fn get_global_model_override(&self) -> Result<Option<ModelId>, ModelOverrideStoreError> {
        Ok(None)
    }

    async fn set_global_model_override(
        &self,
        _model: &ModelId,
    ) -> Result<(), ModelOverrideStoreError> {
        Ok(())
    }

    async fn clear_global_model_override(&self) -> Result<(), ModelOverrideStoreError> {
        Ok(())
    }
}

/// Default kernel reasoning-effort override store: no override and no
/// persistence.
#[derive(Debug, Default)]
pub struct NoopGlobalReasoningEffortOverrideStore;

#[async_trait]
impl GlobalReasoningEffortOverrideStore for NoopGlobalReasoningEffortOverrideStore {
    async fn get_global_reasoning_effort_override(
        &self,
    ) -> Result<Option<ReasoningEffort>, ReasoningEffortOverrideStoreError> {
        Ok(None)
    }

    async fn set_global_reasoning_effort_override(
        &self,
        _effort: ReasoningEffort,
    ) -> Result<(), ReasoningEffortOverrideStoreError> {
        Ok(())
    }

    async fn clear_global_reasoning_effort_override(
        &self,
    ) -> Result<(), ReasoningEffortOverrideStoreError> {
        Ok(())
    }
}

/// Source that won the model resolution hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedSource {
    /// The configured default model was used.
    ConfigDefault,
    /// The global override was used.
    GlobalOverride,
    /// The session override was used.
    SessionOverride,
    /// The explicit per-call argument was used.
    Explicit,
}

impl ResolvedSource {
    /// Stable JSON/string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConfigDefault => "config-default",
            Self::GlobalOverride => "global-override",
            Self::SessionOverride => "session-override",
            Self::Explicit => "explicit-arg",
        }
    }
}

/// Resolved model metadata plus the hierarchy source that selected it.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModelWithSource {
    pub info: ModelInfo,
    pub source: ResolvedSource,
}

/// Source that won the reasoning-effort resolution hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortSource {
    /// The selected model capability default applies.
    ModelDefault,
    /// The global override was used.
    GlobalOverride,
    /// The session override was used.
    SessionOverride,
    /// The explicit per-call argument was used.
    Explicit,
}

impl EffortSource {
    /// Stable JSON/string representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelDefault => "model-default",
            Self::GlobalOverride => "global-override",
            Self::SessionOverride => "session-override",
            Self::Explicit => "explicit-arg",
        }
    }
}

/// Resolved reasoning-effort value plus the hierarchy source that selected it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedEffort {
    pub effort: Option<ReasoningEffort>,
    pub source: EffortSource,
}
