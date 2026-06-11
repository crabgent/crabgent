//! Configurable `Provider` test double.
//!
//! [`StubProvider`] folds the behaviours that were re-implemented per crate and
//! per test file into one builder: a canned single response, a scripted
//! sequence consumed by an internal counter, failure injection (always or on a
//! specific call), configurable capabilities/models/name, plus call-count and
//! captured-request introspection.
//!
//! The default [`Provider::stream`] impl (synthesising a stream from
//! `complete`) is left untouched, which covers every surveyed streaming call
//! site; failure and scripting therefore flow through `complete` and surface in
//! the synthetic stream as well.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_core::model::ModelInfo;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse};
use crabgent_core::{ProviderError, RunCtx};
use tokio_util::sync::CancellationToken;

/// What `complete` should return for the next call.
enum Script {
    /// Same response on every call.
    Canned(LlmResponse),
    /// One response per call, consumed front-to-back. Exhaustion returns
    /// [`ProviderError::Other`] (`"script exhausted"`).
    Sequence(Mutex<std::collections::VecDeque<LlmResponse>>),
}

/// Factory for an injected failure.
///
/// `ProviderError` is deliberately not `Clone` (it is a `thiserror` enum with
/// non-cloneable sources), so a stub that fails on more than one call stores a
/// closure that re-builds the error per call instead of cloning a stored value.
type FailFactory = Box<dyn Fn() -> ProviderError + Send + Sync>;

/// A configurable [`Provider`] double for kernel-driving tests.
///
/// Construct with [`StubProvider::new`] (echoes `"ok"`) or one of the
/// shorthand constructors, then refine with the builder methods. Every
/// configuration option is chainable.
///
/// ```
/// use crabgent_test_support::{StubProvider, done};
///
/// let provider = StubProvider::new()
///     .responses(vec![done("first"), done("second")])
///     .with_tools(true);
/// assert_eq!(provider.call_count(), 0);
/// ```
pub struct StubProvider {
    name: &'static str,
    capabilities: ProviderCapabilities,
    models: Vec<ModelInfo>,
    script: Script,
    /// `Some((n, factory))` fails on the 1-based call `n`; `n == 0` (the
    /// [`Self::fail_with`] shorthand) fails on every call.
    failure: Option<(usize, FailFactory)>,
    calls: AtomicUsize,
    captured: Mutex<Vec<LlmRequest>>,
}

impl Default for StubProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl StubProvider {
    /// A stub that returns a single `"ok"` end-of-turn response and registers
    /// one permissive model (`minimal("m", "stub")`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            name: "stub",
            capabilities: ProviderCapabilities::default(),
            models: vec![ModelInfo::minimal("m", "stub")],
            script: Script::Canned(crate::builders::done("ok")),
            failure: None,
            calls: AtomicUsize::new(0),
            captured: Mutex::new(Vec::new()),
        }
    }

    /// A stub whose single canned response carries `text`.
    #[must_use]
    pub fn with_text(text: impl Into<String>) -> Self {
        Self::new().response(crate::builders::done(text))
    }

    /// Set the single canned response returned on every `complete` call.
    #[must_use]
    pub fn response(mut self, response: LlmResponse) -> Self {
        self.script = Script::Canned(response);
        self
    }

    /// Set a scripted response sequence, one entry consumed per `complete`
    /// call. Once the script is empty, `complete` returns
    /// [`ProviderError::Other`].
    #[must_use]
    pub fn responses(mut self, responses: Vec<LlmResponse>) -> Self {
        self.script = Script::Sequence(Mutex::new(responses.into()));
        self
    }

    /// Fail every `complete` call with the error built by `factory`.
    ///
    /// `ProviderError` is not `Clone`, so the error is supplied as a closure
    /// that runs once per failing call, e.g.
    /// `.fail_with(|| ProviderError::Other("boom".into()))`.
    #[must_use]
    pub fn fail_with<F>(mut self, factory: F) -> Self
    where
        F: Fn() -> ProviderError + Send + Sync + 'static,
    {
        self.failure = Some((0, Box::new(factory)));
        self
    }

    /// Fail the 1-based `call`-th `complete` call with the error built by
    /// `factory`; other calls run the configured script.
    #[must_use]
    pub fn fail_on<F>(mut self, call: usize, factory: F) -> Self
    where
        F: Fn() -> ProviderError + Send + Sync + 'static,
    {
        self.failure = Some((call, Box::new(factory)));
        self
    }

    /// Replace the advertised provider capabilities wholesale.
    ///
    /// Named `with_capabilities` (not `capabilities`) so it never shadows the
    /// inherent [`Provider::capabilities`] trait method on the same type.
    #[must_use]
    pub const fn with_capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Toggle the `tools` capability flag, leaving the rest in place.
    #[must_use]
    pub const fn with_tools(mut self, tools: bool) -> Self {
        self.capabilities.tools = tools;
        self
    }

    /// Replace the advertised model catalog.
    ///
    /// Named `with_models` (not `models`) so it never shadows the inherent
    /// [`Provider::models`] trait method on the same type.
    #[must_use]
    pub fn with_models(mut self, models: Vec<ModelInfo>) -> Self {
        self.models = models;
        self
    }

    /// Override the provider name returned by [`Provider::name`].
    #[must_use]
    pub const fn with_name(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// Number of `complete` calls observed so far.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Snapshot of every `LlmRequest` seen by `complete`, in call order.
    #[must_use]
    pub fn captured_requests(&self) -> Vec<LlmRequest> {
        self.captured
            .lock()
            .expect("captured-requests mutex must not be poisoned")
            .clone()
    }

    fn next_response(&self) -> Result<LlmResponse, ProviderError> {
        match &self.script {
            Script::Canned(response) => Ok(response.clone()),
            Script::Sequence(queue) => {
                let mut guard = match queue.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                guard
                    .pop_front()
                    .ok_or_else(|| ProviderError::Other("script exhausted".into()))
            }
        }
    }
}

#[async_trait]
impl Provider for StubProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.captured
            .lock()
            .expect("captured-requests mutex must not be poisoned")
            .push(req.clone());

        if let Some((on, factory)) = &self.failure
            && (*on == 0 || *on == call)
        {
            return Err(factory());
        }

        self.next_response()
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities.clone()
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }
}
