//! `Kernel` and the typestate `KernelBuilder`.
//!
//! `KernelBuilder` is parameterised over two marker types tracking
//! whether at least one provider and the policy hook have been set.
//! `build()` exists only when both are `Set`. Calling `build()` on a
//! partially configured builder is a compile error, not a runtime panic.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::hook::Hook;
use crate::hook_chain::HookChain;
use crate::model::{
    GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, ModelRegistry,
    NoopGlobalModelOverrideStore, NoopGlobalReasoningEffortOverrideStore,
};
use crate::policy::PolicyHook;
use crate::provider::Provider;
use crate::provider_set::{BuildError, ProviderSet};
use crate::tool::Tool;

#[cfg(test)]
mod builder_tests;
mod global_override;
mod shutdown;

/// Default kernel limits.
#[derive(Debug, Clone)]
pub struct Defaults {
    pub max_turns: u32,
}

impl Defaults {
    /// Default size of the streaming event buffer.
    pub const STREAM_BUFFER_SIZE: usize = 64;

    pub const DEFAULT_MAX_TURNS: u32 = 50;

    /// Default grace period for `Kernel::shutdown` before active runs are
    /// force-aborted.
    pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            max_turns: Self::DEFAULT_MAX_TURNS,
        }
    }
}

/// Marker trait for typestate slots.
pub trait BuilderState {}

/// Slot is unset.
pub struct Unset;

/// Slot is set.
pub struct Set;

impl BuilderState for Unset {}
impl BuilderState for Set {}

/// Configured kernel ready to run.
pub struct Kernel {
    pub(crate) providers: Arc<ProviderSet>,
    pub(crate) policy: Arc<dyn PolicyHook>,
    pub(crate) tools: Vec<Arc<dyn Tool>>,
    pub(crate) hooks: HookChain,
    pub(crate) defaults: Defaults,
    pub(crate) models: Arc<ModelRegistry>,
    pub(crate) global_override_store: Arc<dyn GlobalModelOverrideStore>,
    pub(crate) global_reasoning_effort_override_store: Arc<dyn GlobalReasoningEffortOverrideStore>,
    /// Kernel-wide shutdown signal. Per-run cancel tokens derived from
    /// `None`-caller calls are direct children; caller-supplied
    /// per-run tokens are wired through a watcher task that bridges
    /// shutdown into the per-run token (the per-run token itself is a
    /// child of the caller-supplied token, so caller cancellation
    /// stays synchronous). New runs spawned after this token is
    /// cancelled return `KernelError::ShuttingDown` immediately.
    pub(crate) shutdown_token: CancellationToken,
    /// Kernel-wide cooperative pause signal. Every per-run pause token
    /// observes it (directly or via a watcher when the caller supplied
    /// its own pause token in `RunRequest.pause`). Unlike
    /// `shutdown_token`, firing it never interrupts in-flight provider
    /// or tool futures: runs exit with `Outcome::Paused` at their next
    /// safe boundary. New runs are still accepted while paused; they
    /// observe the signal at their first turn boundary.
    pub(crate) pause_token: CancellationToken,
    /// Tracked driver tasks of currently active `run_streaming` invocations.
    /// `Kernel::shutdown` drains this set within the configured grace
    /// window before aborting any leftover tasks.
    pub(crate) running: Arc<Mutex<JoinSet<()>>>,
    /// Grace period after `shutdown_token` is fired before active runs
    /// are force-aborted. Defaults to [`Defaults::DEFAULT_SHUTDOWN_GRACE`].
    pub(crate) shutdown_grace: Duration,
}

impl Kernel {
    /// Start a new builder.
    #[must_use]
    pub fn builder() -> KernelBuilder<Unset, Unset> {
        KernelBuilder::new()
    }

    /// Look up a registered tool by name.
    #[must_use]
    pub fn tool(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name)
    }

    /// Provider name (for logging and debug).
    #[must_use]
    pub fn provider_name(&self) -> &str {
        self.providers.primary().name()
    }

    /// Number of providers registered on this kernel.
    #[must_use]
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// Number of registered tools.
    #[must_use]
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Number of registered hooks.
    #[must_use]
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    /// Borrow the kernel defaults.
    #[must_use]
    pub const fn defaults(&self) -> &Defaults {
        &self.defaults
    }

    /// Borrow the registered provider.
    #[must_use]
    pub fn provider(&self) -> &Arc<dyn Provider> {
        self.providers.primary()
    }

    /// Borrow all registered providers in registration order.
    #[must_use]
    pub fn providers(&self) -> &[Arc<dyn Provider>] {
        self.providers.all()
    }

    /// Borrow the registered policy hook.
    #[must_use]
    pub fn policy(&self) -> &Arc<dyn PolicyHook> {
        &self.policy
    }

    /// Borrow the hook chain.
    #[must_use]
    pub const fn hooks(&self) -> &HookChain {
        &self.hooks
    }

    /// Borrow the registered tools.
    #[must_use]
    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }

    /// Borrow the registry of models the configured provider serves.
    /// The run-loop validates `LlmRequest.model` against this registry
    /// before every `Provider::complete()` call.
    #[must_use]
    pub fn models(&self) -> &ModelRegistry {
        &self.models
    }

    /// Kernel-wide shutdown token. Child of this token is installed on
    /// every per-run cancel token; cancelling it stops all in-flight
    /// runs cooperatively. Returned by reference so adapters and tests
    /// can clone it.
    #[must_use]
    pub const fn shutdown_token(&self) -> &CancellationToken {
        &self.shutdown_token
    }

    /// Kernel-wide cooperative pause token. Observed by every per-run
    /// pause token; see [`Kernel::request_pause`]. Returned by reference
    /// so adapters and tests can clone it.
    #[must_use]
    pub const fn pause_token(&self) -> &CancellationToken {
        &self.pause_token
    }

    /// Configured shutdown grace period.
    #[must_use]
    pub const fn shutdown_grace(&self) -> Duration {
        self.shutdown_grace
    }
}

/// Typestate builder.
///
/// Compile-time invariants:
/// - at least one provider must be set before `build()` compiles.
/// - `policy` must be set before `build()` compiles.
///
/// ```compile_fail
/// use crabgent_core::Kernel;
/// // Cannot build without provider and policy.
/// let _ = Kernel::builder().build();
/// ```
///
/// ```compile_fail
/// use crabgent_core::{Kernel, AllowAllPolicy, RunCtx};
/// // Cannot build without provider, even if policy is set.
/// let _ = Kernel::builder().policy(AllowAllPolicy).build();
/// ```
pub struct KernelBuilder<P: BuilderState, Pol: BuilderState> {
    providers: Vec<Arc<dyn Provider>>,
    policy: Option<Arc<dyn PolicyHook>>,
    tools: Vec<Arc<dyn Tool>>,
    hooks: HookChain,
    defaults: Defaults,
    global_override_store: Arc<dyn GlobalModelOverrideStore>,
    global_reasoning_effort_override_store: Arc<dyn GlobalReasoningEffortOverrideStore>,
    shutdown_grace: Duration,
    _state: PhantomData<(P, Pol)>,
}

impl Default for KernelBuilder<Unset, Unset> {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelBuilder<Unset, Unset> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            policy: None,
            tools: Vec::new(),
            hooks: HookChain::new(),
            defaults: Defaults::default(),
            global_override_store: Arc::new(NoopGlobalModelOverrideStore),
            global_reasoning_effort_override_store: Arc::new(
                NoopGlobalReasoningEffortOverrideStore,
            ),
            shutdown_grace: Defaults::DEFAULT_SHUTDOWN_GRACE,
            _state: PhantomData,
        }
    }
}

impl<P: BuilderState, Pol: BuilderState> KernelBuilder<P, Pol> {
    /// Register one provider. Repeat this call to attach multiple
    /// providers; `RunRequest.model` may stay unqualified when its id is
    /// unique or use `ModelTarget::new(provider, model)` to pin a provider.
    #[must_use]
    pub fn provider<Prov: Provider + 'static>(mut self, provider: Prov) -> KernelBuilder<Set, Pol> {
        self.providers.push(Arc::new(provider));
        KernelBuilder {
            providers: self.providers,
            policy: self.policy,
            tools: self.tools,
            hooks: self.hooks,
            defaults: self.defaults,
            global_override_store: self.global_override_store,
            global_reasoning_effort_override_store: self.global_reasoning_effort_override_store,
            shutdown_grace: self.shutdown_grace,
            _state: PhantomData,
        }
    }

    /// Register a tool. Available regardless of typestate.
    #[must_use]
    pub fn add_tool<T: Tool + 'static>(mut self, tool: T) -> Self {
        self.tools.push(Arc::new(tool));
        self
    }

    /// Register a hook. Available regardless of typestate.
    #[must_use]
    pub fn add_hook<H: Hook + 'static>(mut self, hook: H) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Override the kernel defaults.
    #[must_use]
    pub const fn defaults(mut self, defaults: Defaults) -> Self {
        self.defaults = defaults;
        self
    }

    /// Configure the grace period [`Kernel::shutdown`] waits for active
    /// runs to finish cooperatively before force-aborting them. Defaults
    /// to [`Defaults::DEFAULT_SHUTDOWN_GRACE`] (5 s).
    #[must_use]
    pub const fn with_graceful_shutdown(mut self, grace: Duration) -> Self {
        self.shutdown_grace = grace;
        self
    }
}

impl<P: BuilderState> KernelBuilder<P, Unset> {
    /// Set the policy hook. Transitions policy state from `Unset` to `Set`.
    #[must_use]
    pub fn policy<Pol: PolicyHook + 'static>(self, pol: Pol) -> KernelBuilder<P, Set> {
        KernelBuilder {
            providers: self.providers,
            policy: Some(Arc::new(pol)),
            tools: self.tools,
            hooks: self.hooks,
            defaults: self.defaults,
            global_override_store: self.global_override_store,
            global_reasoning_effort_override_store: self.global_reasoning_effort_override_store,
            shutdown_grace: self.shutdown_grace,
            _state: PhantomData,
        }
    }
}

impl<P: BuilderState> KernelBuilder<P, Set> {
    /// Borrow the policy hook already registered on this builder.
    ///
    /// Extension crates that add tools during builder construction use this
    /// to keep tool-side policy checks aligned with the kernel policy.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "KernelBuilder<P, Set> typestate guarantees policy is present"
    )]
    pub fn policy_hook(&self) -> &Arc<dyn PolicyHook> {
        self.policy
            .as_ref()
            .expect("typestate invariant: Set means policy is Some")
    }
}

impl KernelBuilder<Set, Set> {
    /// Build the kernel and panic if provider catalog validation fails.
    ///
    /// Use [`Self::try_build`] in production paths that need to surface a
    /// recoverable configuration error to callers.
    ///
    /// Collects `Provider::models()` into a [`ModelRegistry`]. Panics
    /// if providers have duplicate names, duplicate model ids, or return
    /// models owned by a different provider.
    ///
    /// [`ModelRegistry`]: crate::model::ModelRegistry
    #[must_use]
    #[expect(
        clippy::panic,
        reason = "build intentionally panics; try_build is the fallible production API"
    )]
    pub fn build(self) -> Kernel {
        self.try_build().unwrap_or_else(|e| panic!("{e}"))
    }

    /// Build the kernel, returning typed configuration errors instead of
    /// panicking when provider catalogs are invalid.
    #[expect(
        clippy::expect_used,
        reason = "KernelBuilder<Set, Set> typestate guarantees policy is present"
    )]
    pub fn try_build(self) -> Result<Kernel, BuildError> {
        let providers = ProviderSet::try_new(self.providers)?;
        let policy = self
            .policy
            .expect("typestate invariant: Set means policy is Some");
        let models = providers.try_build_model_registry()?;
        Ok(Kernel {
            providers: Arc::new(providers),
            policy,
            tools: self.tools,
            hooks: self.hooks,
            defaults: self.defaults,
            global_override_store: self.global_override_store,
            global_reasoning_effort_override_store: self.global_reasoning_effort_override_store,
            models: Arc::new(models),
            shutdown_token: CancellationToken::new(),
            pause_token: CancellationToken::new(),
            running: Arc::new(Mutex::new(JoinSet::new())),
            shutdown_grace: self.shutdown_grace,
        })
    }
}
