//! Error hierarchy for the kernel, providers, and tools.

use thiserror::Error;

use crate::model::{ModelId, ModelTarget};

/// Top-level kernel error returned from `Kernel::run()`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum KernelError {
    /// Provider call failed.
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    /// Tool execution failed.
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),

    /// A hook short-circuited the chain with `Decision::Deny`.
    #[error("hook denied: {reason}")]
    HookDenied { reason: String },

    /// The policy hook denied the action.
    #[error("policy denied: {reason}")]
    PolicyDenied { reason: String },

    /// The loop reached `max_turns` without converging.
    #[error("max turns exceeded: {0}")]
    MaxTurnsExceeded(u32),

    /// Too many tools were advertised for the provider request.
    #[error("tool count {count} exceeds advertised limit {max}")]
    TooManyTools { count: usize, max: usize },

    /// The caller cancelled the run.
    #[error("cancelled")]
    Cancelled,

    /// The run stopped cooperatively at a safe boundary (turn start or
    /// between tool dispatches) because a pause was requested via
    /// [`Kernel::request_pause`] or a caller-supplied `RunRequest.pause`
    /// token. Distinct from `Cancelled`: the conversation state at exit
    /// is a clean resume point, not an interrupted one.
    ///
    /// [`Kernel::request_pause`]: crate::Kernel::request_pause
    #[error("paused")]
    Paused,

    /// The kernel has been shut down via [`Kernel::shutdown`] and no
    /// longer accepts new runs. Distinct from `Cancelled` so adapters
    /// can distinguish caller-side cancellation from kernel-wide drain.
    ///
    /// [`Kernel::shutdown`]: crate::Kernel::shutdown
    #[error("kernel shutting down")]
    ShuttingDown,

    /// `RunRequest.model` is not registered with any provider attached
    /// to this kernel. Validation is fail-closed: the run never reaches
    /// the provider with an unknown id.
    #[error("unknown model: {0}")]
    UnknownModel(ModelId),

    /// `RunRequest.model` is an unqualified id that multiple providers
    /// advertise. Use a provider-qualified [`ModelTarget`] to choose one.
    #[error("ambiguous model: {0}")]
    AmbiguousModel(ModelId),

    /// A provider/model fallback target is not registered with any provider
    /// attached to this kernel.
    #[error("unknown model target: {0}")]
    UnknownModelTarget(ModelTarget),

    /// A persisted model override is no longer registered with any provider
    /// attached to this kernel.
    #[error("unknown {scope} model override: {model}")]
    UnknownModelOverride { scope: &'static str, model: ModelId },

    /// Reading a persisted model override failed.
    #[error("model override store error: {reason}")]
    ModelOverrideStore { reason: String },

    /// Reading a persisted reasoning-effort override failed.
    #[error("reasoning effort override store error: {reason}")]
    ReasoningEffortOverrideStore { reason: String },

    /// An internal invariant was violated.
    #[error("internal: {0}")]
    Internal(String),
}

/// Errors a `Provider` implementation can return.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// Network or transport-layer failure.
    #[error("transport error: {0}")]
    Transport(String),

    /// The provider returned an HTTP error.
    #[error("api error: {status} {message}")]
    Api {
        status: u16,
        message: String,
        retry_after_secs: Option<u64>,
    },

    /// Authentication failure (bad credentials, missing API key, etc.).
    #[error("auth error: {0}")]
    Auth(String),

    /// The provider rate-limited the request.
    #[error("rate limited: retry after {retry_after_secs:?}s")]
    RateLimited { retry_after_secs: Option<u64> },

    /// Response body could not be parsed.
    #[error("malformed response: {0}")]
    MalformedResponse(String),

    /// Model discovery failed before a provider call.
    #[error("model discovery failed: {reason}")]
    ModelDiscovery { reason: String },

    /// The selected provider or model cannot accept tool definitions.
    #[error("tools not supported by provider '{provider}' model '{model}'")]
    ToolsUnsupported { provider: String, model: String },

    /// The selected provider or model cannot accept image content.
    #[error("vision not supported by provider '{provider}' model '{model}'")]
    VisionUnsupported { provider: String, model: String },

    /// The selected provider or model cannot accept audio content.
    #[error("audio not supported by provider '{provider}' model '{model}'")]
    AudioUnsupported { provider: String, model: String },

    /// The selected provider or model cannot accept a reasoning-effort setting.
    #[error("reasoning effort not supported by provider '{provider}' model '{model}'")]
    ReasoningEffortUnsupported { provider: String, model: String },

    /// The selected provider or model does not support hosted web search.
    #[error("web search not supported by provider '{provider}' model '{model}'")]
    WebSearchUnsupported { provider: String, model: String },

    /// A streaming response failed after opening, but before committing a
    /// complete provider turn, and can be retried on a fallback provider.
    #[error("retryable stream error: {message}")]
    RetryableStream { message: String },

    /// The cancellation token fired during the call.
    #[error("cancelled")]
    Cancelled,

    /// The provider call exceeded its configured deadline.
    #[error("timeout")]
    Timeout,

    /// Anything else.
    #[error("other: {0}")]
    Other(String),
}

/// Errors a `Tool` implementation can return.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ToolError {
    /// The tool args failed to deserialize or validate.
    #[error("invalid args: {0}")]
    InvalidArgs(String),

    /// The tool ran but the operation failed.
    ///
    /// Workspace audit (sweep-3, 2026-05-21): every production call site
    /// emitting `Execution` is operator-level (store unavailable, executor
    /// spawn failure, missing registry wiring, transient external rate-limit).
    /// None are LLM-recoverable: passing different args cannot fix a store
    /// outage. Tools therefore propagate `Execution` as a hard kernel error
    /// and rely on the operator (logs, alerts) to repair. LLM-recoverable
    /// cases (bad path, missing target, policy deny) use `NotFound` or
    /// `Permission` and route through the run-loop soft-result mapping to
    /// `ToolResult::soft_error`; builtin file tools additionally soft-wrap
    /// recoverable `Io` failures.
    ///
    /// Error-string contents are opaque by convention: the underlying
    /// `StoreError` / `ChannelError` display is never embedded in the
    /// LLM-visible message. Call sites use [`ToolError::backend_unavailable`]
    /// to emit only a short op-label to the LLM.
    #[error("execution failed: {0}")]
    Execution(String),

    /// An I/O failure (file not found, permission denied, network, etc.).
    #[error("io error: {0}")]
    Io(String),

    /// The cancellation token fired mid-execution.
    #[error("cancelled")]
    Cancelled,

    /// The named target was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// A tool-internal policy decision denied the operation. Tools that
    /// gate their own typed actions (e.g. `Action::MemorySearch`) raise
    /// this when `PolicyHook::allow` returns `Deny`.
    #[error("permission denied: {0}")]
    Permission(String),
}

impl ToolError {
    /// Build an opaque execution error for backend/store/provider failures.
    ///
    /// The returned message is safe to surface outside the process. Backend
    /// detail is intentionally ignored here: core does not log directly, and
    /// callers must not forward raw backend errors to the LLM.
    pub fn backend_unavailable(
        op: impl Into<String>,
        _err: &(impl std::fmt::Display + ?Sized),
    ) -> Self {
        let op = op.into();
        Self::Execution(format!("{op}: backend unavailable"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_converts_to_kernel_error() {
        let pe = ProviderError::Auth("bad token".into());
        let ke: KernelError = pe.into();
        assert!(matches!(ke, KernelError::Provider(_)));
    }

    #[test]
    fn tool_error_converts_to_kernel_error() {
        let te = ToolError::Execution("boom".into());
        let ke: KernelError = te.into();
        assert!(matches!(ke, KernelError::Tool(_)));
    }

    #[test]
    fn display_includes_status_and_message() {
        let e = ProviderError::Api {
            status: 429,
            message: "too many requests".into(),
            retry_after_secs: Some(5),
        };
        let s = e.to_string();
        assert!(s.contains("429"), "missing status: {s}");
        assert!(s.contains("too many requests"), "missing message: {s}");
    }

    #[test]
    fn hook_denied_carries_reason() {
        let e = KernelError::HookDenied {
            reason: "rate limit".into(),
        };
        assert!(e.to_string().contains("rate limit"));
    }

    #[test]
    fn policy_denied_carries_reason() {
        let e = KernelError::PolicyDenied {
            reason: "no shell".into(),
        };
        assert!(e.to_string().contains("no shell"));
    }

    #[test]
    fn max_turns_carries_count() {
        let e = KernelError::MaxTurnsExceeded(50);
        assert!(e.to_string().contains("50"));
    }

    #[test]
    fn cancelled_renders_short() {
        assert_eq!(KernelError::Cancelled.to_string(), "cancelled");
        assert_eq!(ProviderError::Cancelled.to_string(), "cancelled");
        assert_eq!(ToolError::Cancelled.to_string(), "cancelled");
    }

    #[test]
    fn rate_limited_optional_retry_after() {
        let with = ProviderError::RateLimited {
            retry_after_secs: Some(30),
        };
        assert!(with.to_string().contains("30"));
        let without = ProviderError::RateLimited {
            retry_after_secs: None,
        };
        assert!(without.to_string().contains("None"));
    }

    #[test]
    fn retryable_stream_carries_message() {
        let err = ProviderError::RetryableStream {
            message: "overloaded_error: busy".into(),
        };
        assert!(err.to_string().contains("busy"));
    }

    #[test]
    fn provider_error_model_discovery_display() {
        let err = ProviderError::ModelDiscovery {
            reason: "models endpoint unavailable".into(),
        };

        assert_eq!(
            err.to_string(),
            "model discovery failed: models endpoint unavailable"
        );
    }

    #[test]
    fn vision_unsupported_display_format() {
        let err = ProviderError::VisionUnsupported {
            provider: "anthropic".into(),
            model: "claude-3-haiku".into(),
        };

        assert_eq!(
            err.to_string(),
            "vision not supported by provider 'anthropic' model 'claude-3-haiku'"
        );
    }

    #[test]
    fn tools_unsupported_display_format() {
        let err = ProviderError::ToolsUnsupported {
            provider: "openai".into(),
            model: "gpt-5.5".into(),
        };

        assert_eq!(
            err.to_string(),
            "tools not supported by provider 'openai' model 'gpt-5.5'"
        );
    }

    #[test]
    fn audio_unsupported_display_format() {
        let err = ProviderError::AudioUnsupported {
            provider: "openai".into(),
            model: "gpt-5.5".into(),
        };

        assert_eq!(
            err.to_string(),
            "audio not supported by provider 'openai' model 'gpt-5.5'"
        );
    }

    #[test]
    fn tool_invalid_args_passes_message() {
        let e = ToolError::InvalidArgs("missing field path".into());
        assert!(e.to_string().contains("missing field path"));
    }

    #[test]
    fn unknown_model_carries_id() {
        let e = KernelError::UnknownModel(ModelId::new("gpt-9"));
        assert!(e.to_string().contains("gpt-9"));
    }

    #[test]
    fn ambiguous_model_carries_id() {
        let e = KernelError::AmbiguousModel(ModelId::new("gpt-9"));
        assert!(e.to_string().contains("gpt-9"));
    }

    #[test]
    fn unknown_model_target_carries_provider_and_id() {
        let e = KernelError::UnknownModelTarget(ModelTarget::new("openai", "gpt-9"));
        let s = e.to_string();
        assert!(s.contains("openai"), "missing provider: {s}");
        assert!(s.contains("gpt-9"), "missing model: {s}");
    }

    #[test]
    fn tool_permission_carries_reason() {
        let e = ToolError::Permission("policy denied".into());
        let s = e.to_string();
        assert!(s.contains("permission denied"), "missing prefix: {s}");
        assert!(s.contains("policy denied"), "missing reason: {s}");
    }

    #[test]
    fn tool_permission_converts_to_kernel_error() {
        let te = ToolError::Permission("scope outside subject".into());
        let ke: KernelError = te.into();
        assert!(matches!(ke, KernelError::Tool(ToolError::Permission(_))));
    }

    #[test]
    fn backend_unavailable_omits_backend_detail() {
        let err = StoreSentinel("sentinel-secret-should-not-leak");
        let mapped = ToolError::backend_unavailable("cron.create", &err);

        let ToolError::Execution(message) = mapped else {
            panic!("expected execution error");
        };
        assert_eq!(message, "cron.create: backend unavailable");
        assert!(!message.contains("sentinel-secret-should-not-leak"));
    }

    struct StoreSentinel(&'static str);

    impl std::fmt::Display for StoreSentinel {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "backend detail: {}", self.0)
        }
    }
}
