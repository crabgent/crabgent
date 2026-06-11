//! Tracing bridge hook for crabgent kernel lifecycle events.

mod hook;

use crabgent_core::HookChain;
#[cfg(feature = "default-bundle")]
use crabgent_core::{BuilderState, KernelBuilder};

pub use hook::{LogHook, LogHookConfig, LogLevel};

/// Build the default logging hook without installing a global subscriber.
///
/// Use this when the host application already owns tracing subscriber setup.
#[must_use]
pub fn default_log_hook() -> LogHook {
    LogHook::default()
}

/// Build a hook chain containing only the default [`LogHook`].
///
/// Use this when a caller wants the curated hook set but still owns subscriber
/// configuration and kernel assembly.
#[must_use]
pub fn default_hook_chain() -> HookChain {
    let mut chain = HookChain::new();
    chain.push(default_log_hook());
    chain
}

/// Install crabgent's opt-in logging defaults on a [`KernelBuilder`].
///
/// This initializes the optional `crabgent-log` subscriber with
/// [`crabgent_log::DEFAULT_DIRECTIVE`], installs the [`tracing_log`] bridge and panic
/// handler from `crabgent-log`, and attaches [`LogHook`] to the returned builder.
/// Hosts that need custom subscriber setup should call [`default_log_hook`] or
/// [`default_hook_chain`] instead.
#[cfg(feature = "default-bundle")]
#[must_use]
pub fn install_defaults<P, Pol>(builder: KernelBuilder<P, Pol>) -> KernelBuilder<P, Pol>
where
    P: BuilderState,
    Pol: BuilderState,
{
    crabgent_log::init_default();
    builder.add_hook(default_log_hook())
}
