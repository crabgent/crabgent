//! [`CompactHook`] implementation.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{
    Decision, Hook, Message, ModelId, Outcome, Provider, ProviderError, RunCtx, RunId,
};
use crabgent_log::{debug, redact_uid, warn};
use crabgent_store::{SessionId, SessionStore, Utc};
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};

use crate::compact_plan::CompactionPlan;
use crate::config::{CompactConfig, CompactFailureMode};
use crate::render::{render_summary_message, render_transcript};
use crate::summary_chain::{CompactError, SummaryAttempt, summarize_with_chain};
use crate::token_count::estimate_tokens;

/// Hook that semantically compacts old conversation messages before provider
/// requests.
pub struct CompactHook {
    attempts: Vec<SummaryAttempt>,
    config: CompactConfig,
    /// Per-run mute set populated when the fallback chain is exhausted on a
    /// non-retryable error. Suppresses repeat WARN-per-turn for runs whose
    /// compaction is permanently broken (e.g. provider 404 on the configured
    /// model). Cleared by `on_stop`.
    muted_runs: Arc<RwLock<HashSet<RunId>>>,
    pre_compact_locks: Arc<Mutex<HashMap<RunId, Arc<Mutex<()>>>>>,
    pre_compact_cache: Arc<Mutex<HashMap<RunId, CachedPreCompact>>>,
    /// Optional session store used for continuity summaries across runs.
    session_store: Option<Arc<dyn SessionStore>>,
}

#[derive(Clone)]
struct CachedPreCompact {
    fingerprint: [u8; 32],
    replacement: Vec<Message>,
}

impl CompactHook {
    /// Create a compaction hook with default thresholds.
    pub fn new<P>(provider: Arc<P>, model: impl Into<ModelId>) -> Self
    where
        P: Provider + 'static,
    {
        Self {
            attempts: vec![SummaryAttempt::new(provider, model)],
            config: CompactConfig::default(),
            muted_runs: Arc::new(RwLock::new(HashSet::new())),
            pre_compact_locks: Arc::new(Mutex::new(HashMap::new())),
            pre_compact_cache: Arc::new(Mutex::new(HashMap::new())),
            session_store: None,
        }
    }

    /// Add a fallback summary provider attempted after earlier providers
    /// return a fallback-eligible error or an empty summary.
    #[must_use]
    pub fn with_fallback<P>(mut self, provider: Arc<P>, model: impl Into<ModelId>) -> Self
    where
        P: Provider + 'static,
    {
        self.attempts.push(SummaryAttempt::new(provider, model));
        self
    }

    /// Replace the full config.
    #[must_use]
    pub fn with_config(mut self, config: CompactConfig) -> Self {
        self.config = config;
        self
    }

    /// Compact once the message count is above `max`.
    #[must_use]
    pub const fn with_max_messages(mut self, max: usize) -> Self {
        self.config.max_messages = max;
        self
    }

    /// Compact once the approximate message token count is above `max`.
    #[must_use]
    pub const fn with_max_tokens(mut self, max: usize) -> Self {
        self.config.max_tokens = max;
        self
    }

    /// Keep this many newest non-leading-system messages verbatim.
    #[must_use]
    pub const fn with_keep_recent_messages(mut self, count: usize) -> Self {
        self.config.keep_recent_messages = count;
        self
    }

    /// Set summary provider max output tokens.
    #[must_use]
    pub const fn with_summary_max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.config.summary_max_tokens = max_tokens;
        self
    }

    /// Set summary provider temperature.
    #[must_use]
    pub const fn with_summary_temperature(mut self, temperature: Option<f32>) -> Self {
        self.config.summary_temperature = temperature;
        self
    }

    /// Persist and load continuity summaries through a session store.
    #[must_use]
    pub fn with_session_store<S: SessionStore + 'static>(mut self, store: Arc<S>) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Set the system prompt used for summary provider calls.
    #[must_use]
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.config.system_prompt = prompt.into();
        self
    }

    /// Set the user instruction appended after the rendered transcript.
    #[must_use]
    pub fn with_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.config.instruction = instruction.into();
        self
    }

    /// Set provider-failure behavior.
    #[must_use]
    pub const fn with_failure_mode(mut self, mode: CompactFailureMode) -> Self {
        self.config.failure_mode = mode;
        self
    }

    /// Borrow the active config.
    #[must_use]
    pub const fn config(&self) -> &CompactConfig {
        &self.config
    }

    fn should_compact(&self, messages: &[Message]) -> bool {
        messages.len() > self.config.max_messages
            || estimate_tokens(messages) > self.config.max_tokens
    }

    async fn acquire_run_lock(&self, run_id: &RunId) -> OwnedMutexGuard<()> {
        let inner = {
            let mut map = self.pre_compact_locks.lock().await;
            map.entry(run_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        inner.lock_owned().await
    }

    async fn cached_pre_compact(
        &self,
        run_id: &RunId,
        fingerprint: Option<&[u8; 32]>,
    ) -> Option<Vec<Message>> {
        let fingerprint = fingerprint?;
        let cache = self.pre_compact_cache.lock().await;
        cache
            .get(run_id)
            .filter(|entry| entry.fingerprint == *fingerprint)
            .map(|entry| entry.replacement.clone())
    }

    async fn store_pre_compact_cache(
        &self,
        run_id: &RunId,
        fingerprint: Option<[u8; 32]>,
        replacement: &[Message],
    ) {
        let Some(fingerprint) = fingerprint else {
            return;
        };
        self.pre_compact_cache.lock().await.insert(
            run_id.clone(),
            CachedPreCompact {
                fingerprint,
                replacement: replacement.to_vec(),
            },
        );
    }

    async fn summarize(
        &self,
        transcript: &str,
        prior_summary: Option<&str>,
        ctx: &RunCtx,
    ) -> Result<String, CompactError> {
        summarize_with_chain(&self.attempts, &self.config, transcript, prior_summary, ctx).await
    }

    async fn load_prior_summary(
        &self,
        ctx: &RunCtx,
        session_id: Option<&SessionId>,
    ) -> Option<String> {
        let Some(store) = &self.session_store else {
            return None;
        };
        let session_id = session_id?;
        match store.get_compaction_summary(session_id).await {
            Ok(summary) => summary,
            Err(error) => {
                warn!(
                    run_id = %ctx.run_id,
                    subject = %redact_uid(ctx.subject.id()),
                    error = %error,
                    "compact hook: failed to load prior compaction summary",
                );
                None
            }
        }
    }

    async fn store_summary(&self, ctx: &RunCtx, session_id: Option<&SessionId>, summary: &str) {
        let Some(store) = &self.session_store else {
            return;
        };
        let Some(session_id) = session_id else {
            return;
        };
        if let Err(error) = store.set_compaction_summary(session_id, summary).await {
            warn!(
                run_id = %ctx.run_id,
                subject = %redact_uid(ctx.subject.id()),
                error = %error,
                "compact hook: failed to store compaction summary",
            );
        }
    }

    async fn archive_compacted(
        &self,
        ctx: &RunCtx,
        session_id: Option<&SessionId>,
        compacted: &[Message],
    ) -> bool {
        let Some(store) = &self.session_store else {
            return true;
        };
        let Some(session_id) = session_id else {
            return true;
        };
        match store
            .archive_messages(session_id, compacted, Utc::now())
            .await
        {
            Ok(_) => true,
            Err(error) => {
                warn!(
                    run_id = %ctx.run_id,
                    subject = %redact_uid(ctx.subject.id()),
                    error = %error,
                    "compact hook: archive failed; aborting compaction",
                );
                self.muted_runs.write().await.insert(ctx.run_id.clone());
                false
            }
        }
    }

    async fn compact_failure(&self, ctx: &RunCtx, error: &CompactError) -> Decision<Vec<Message>> {
        warn_compact_failure(ctx, error);
        self.mute_run(ctx).await;
        self.failure(error.reason())
    }

    async fn mute_run(&self, ctx: &RunCtx) {
        self.muted_runs.write().await.insert(ctx.run_id.clone());
    }

    fn failure(&self, reason: &str) -> Decision<Vec<Message>> {
        match self.config.failure_mode {
            CompactFailureMode::Continue => Decision::Continue,
            CompactFailureMode::Deny => Decision::Deny(reason.into()),
        }
    }
}

fn warn_compact_failure(ctx: &RunCtx, error: &CompactError) {
    if matches!(error, CompactError::Provider(ProviderError::Auth(_))) {
        warn_compact_auth_failure(ctx);
        return;
    }
    warn_compact_general_failure(ctx, error);
}

fn warn_compact_auth_failure(ctx: &RunCtx) {
    warn!(
        run_id = %ctx.run_id,
        subject = %redact_uid(ctx.subject.id()),
        "compact hook: provider auth error (details redacted)",
    );
}

fn warn_compact_general_failure(ctx: &RunCtx, error: &CompactError) {
    warn!(
        run_id = %ctx.run_id,
        subject = %redact_uid(ctx.subject.id()),
        error = %error,
        "compact hook: compaction failed",
    );
}

pub fn session_id(ctx: &RunCtx) -> Option<SessionId> {
    let id = ctx.session_id()?;
    match SessionId::from_str(id) {
        Ok(session_id) => Some(session_id),
        Err(error) => {
            warn!(
                error = %error,
                "compact hook: malformed session_id, skipping store ops",
            );
            None
        }
    }
}

#[async_trait]
impl Hook for CompactHook {
    async fn pre_compact(&self, messages: &[Message], ctx: &RunCtx) -> Decision<Vec<Message>> {
        if !self.should_compact(messages) {
            return Decision::Continue;
        }
        if self.muted_runs.read().await.contains(&ctx.run_id) {
            return Decision::Continue;
        }
        let _guard = self.acquire_run_lock(&ctx.run_id).await;
        if self.muted_runs.read().await.contains(&ctx.run_id) {
            return Decision::Continue;
        }
        let fingerprint = message_fingerprint(messages);
        if let Some(replacement) = self
            .cached_pre_compact(&ctx.run_id, fingerprint.as_ref())
            .await
        {
            return Decision::Replace(replacement);
        }

        let session_id = session_id(ctx);
        let prior_summary = self.load_prior_summary(ctx, session_id.as_ref()).await;
        let Some(plan) = CompactionPlan::new(messages, self.config.keep_recent_messages) else {
            debug!(
                run_id = %ctx.run_id,
                subject = %redact_uid(ctx.subject.id()),
                message_count = messages.len(),
                keep_recent_messages = self.config.keep_recent_messages,
                "compact hook: partition window too small to compact",
            );
            return Decision::Continue;
        };
        if !self
            .archive_compacted(ctx, session_id.as_ref(), plan.compacted)
            .await
        {
            return Decision::Continue;
        }
        let transcript = render_transcript(plan.compacted);
        let summary = match self
            .summarize(&transcript, prior_summary.as_deref(), ctx)
            .await
        {
            Ok(summary) => summary,
            Err(error) => return self.compact_failure(ctx, &error).await,
        };
        self.store_summary(ctx, session_id.as_ref(), &summary).await;

        let mut next = Vec::with_capacity(
            plan.leading_system.len()
                + usize::from(plan.prior_summary.is_some())
                + 1
                + plan.recent.len(),
        );
        next.extend_from_slice(plan.leading_system);
        if let Some(prior) = plan.prior_summary {
            next.push(prior.clone());
        }
        next.push(render_summary_message(&summary, plan.compacted.len()));
        next.extend_from_slice(plan.recent);
        self.store_pre_compact_cache(&ctx.run_id, fingerprint, &next)
            .await;

        debug!(
            run_id = %ctx.run_id,
            subject = %redact_uid(ctx.subject.id()),
            compacted_messages = plan.compacted.len(),
            kept_recent_messages = plan.recent.len(),
            "compact hook: replaced provider-facing message window",
        );
        Decision::Replace(next)
    }

    async fn on_stop(&self, ctx: &RunCtx, _outcome: &Outcome) {
        self.muted_runs.write().await.remove(&ctx.run_id);
        self.pre_compact_cache.lock().await.remove(&ctx.run_id);
        self.pre_compact_locks.lock().await.remove(&ctx.run_id);
    }
}

fn message_fingerprint(messages: &[Message]) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};

    let serialized = serde_json::to_string(messages).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(serialized.as_bytes());
    Some(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::Subject;
    use crabgent_test_support::StubProvider;

    fn ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("u"))
    }

    #[test]
    fn pub_session_id_returns_none_on_malformed() {
        let ctx = ctx();
        ctx.set_session_id("not-a-session-id")
            .expect("session id cell starts empty");

        assert!(session_id(&ctx).is_none());
    }

    #[tokio::test]
    async fn compact_failure_redacts_auth_error() {
        let hook = CompactHook::new(Arc::new(StubProvider::new()), "summary-model");
        let error = CompactError::Provider(ProviderError::Auth("token-leak-x".into()));

        let decision = hook.compact_failure(&ctx(), &error).await;

        assert!(matches!(decision, Decision::Continue));
    }
}
