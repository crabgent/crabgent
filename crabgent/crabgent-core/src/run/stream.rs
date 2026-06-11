//! Streaming run-loop driver. Spawns a background task that drives the
//! agentic loop and emits `Event` values on an mpsc channel. The public
//! API in `run::mod` wraps the receiver as a `Stream`.
//!
//! This is the single run-loop implementation used by both `run()` and
//! `run_streaming()`. It consumes `Provider::stream()`; providers without
//! native streaming use the default stream wrapper around `complete()`.
//! Shared helpers live in `run/shared.rs`.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::KernelError;
use crate::hook::{CancelReason, Event, Outcome, RunCtx};
use crate::hook_chain::HookChain;
use crate::message::{Message, RawMessages};
use crate::model::{
    EffortSource, GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, ModelId,
    ModelRegistry, ModelTarget, ReasoningEffort, ResolvedEffort, ResolvedModelWithSource,
};
use crate::policy::PolicyHook;
use crate::provider_set::ProviderSet;
use crate::tool::Tool;
use crate::types::{ToolCall, WebSearchConfig};

use super::model_resolution::resolve_model_target_with_overrides;
use super::shared::{check_cancel, check_pause, resolve_tool_call, tool_defs};

pub(in crate::run) mod events;
mod provider_turn;
mod turn;
use events::{emit_completed, emit_event, prepare_event, send_stream_item};
use turn::stream_one_turn;

/// Owned configuration for a streaming run. All `Arc<dyn _>` fields are
/// cheap clones; `HookChain` clones the underlying `Vec<Arc<dyn Hook>>`.
pub(super) struct StreamCfg {
    pub providers: Arc<ProviderSet>,
    pub policy: Arc<dyn PolicyHook>,
    pub tools: Vec<Arc<dyn Tool>>,
    pub hooks: HookChain,
    pub run_ctx: RunCtx,
    pub max_turns: u32,
    pub model: ModelTarget,
    pub explicit_model: Option<ModelTarget>,
    pub session_model_override: Option<ModelId>,
    pub fallbacks: Vec<ModelTarget>,
    pub system_prompt: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub web_search: WebSearchConfig,
    pub models: Arc<ModelRegistry>,
    pub global_override_store: Arc<dyn GlobalModelOverrideStore>,
    pub global_reasoning_effort_override_store: Arc<dyn GlobalReasoningEffortOverrideStore>,
    pub cancel: CancellationToken,
    /// Cooperative pause signal, polled at safe boundaries only (never
    /// raced in a `select!`). See [`check_pause`].
    pub pause: CancellationToken,
    /// Clone of the kernel-wide shutdown token, used by `finish_error`
    /// to attribute otherwise-unattributed cancellation to shutdown.
    pub shutdown: CancellationToken,
}

#[derive(Default)]
struct MessageLog {
    typed: Vec<Message>,
    raw: Vec<Value>,
}

impl MessageLog {
    fn from_typed(typed: Vec<Message>) -> Self {
        let raw = RawMessages::from(typed.clone()).into_inner();
        Self { typed, raw }
    }

    async fn from_initial(
        chain: &HookChain,
        ctx: &RunCtx,
        initial: Vec<Message>,
    ) -> Result<Self, KernelError> {
        let prepared = chain.apply_on_user_prompt_submit(&initial, ctx).await?;
        let mut log = Self::default();
        for msg in prepared {
            log.append(chain, ctx, msg).await?;
        }
        Ok(log)
    }

    fn raw(&self) -> &[Value] {
        &self.raw
    }

    async fn compacted_for_provider(
        &self,
        chain: &HookChain,
        ctx: &RunCtx,
    ) -> Result<Option<Self>, KernelError> {
        Ok(chain
            .apply_pre_compact(&self.typed, ctx)
            .await?
            .map(Self::from_typed))
    }

    async fn append(
        &mut self,
        chain: &HookChain,
        ctx: &RunCtx,
        msg: Message,
    ) -> Result<(), KernelError> {
        self.typed.push(msg);
        self.typed = chain.apply_on_message(&self.typed, ctx).await?;
        self.raw = RawMessages::from(self.typed.clone()).into_inner();
        Ok(())
    }
}

/// Drive the streaming loop end-to-end. Sends `Ok(Event::Final(text))` on
/// success or `Err(KernelError)` on failure as the last channel message.
pub(super) async fn drive_stream(
    cfg: StreamCfg,
    initial: Vec<Message>,
    tx: mpsc::Sender<Result<Event, KernelError>>,
) {
    let result = tokio::select! {
        biased;
        () = cfg.cancel.cancelled() => Err(KernelError::Cancelled),
        () = tx.closed() => Err(KernelError::Cancelled),
        result = drive_inner(&cfg, initial, Some(&cfg.cancel), &tx) => result,
    };
    match result {
        Ok(text) => finish_success(&cfg, text, &tx).await,
        Err(err) => finish_error(&cfg, err, &tx).await,
    }
}

fn outcome_for_error(err: &KernelError) -> Outcome {
    match err {
        KernelError::MaxTurnsExceeded(_) => Outcome::MaxTurnsExceeded,
        KernelError::Cancelled => Outcome::Cancelled,
        KernelError::Paused => Outcome::Paused,
        other => Outcome::Errored(other.to_string()),
    }
}

/// Both success arms call `on_stop` before sending the terminal stream item.
/// That order keeps the persisted outcome ahead of `Event::Final` or the
/// hook error that wakes stream consumers. Consumers can then react to the
/// terminal channel item without observing stale run outcome state.
/// In the hook-error arm, `on_stop` still receives `Outcome::Completed`
/// because the run itself succeeded; the hook error is an interception after
/// the provider turn completed.
async fn finish_success(
    cfg: &StreamCfg,
    text: String,
    tx: &mpsc::Sender<Result<Event, KernelError>>,
) {
    match prepare_event(&cfg.hooks, &cfg.run_ctx, Event::Final(text.clone())).await {
        Ok(event) => {
            cfg.hooks
                .apply_on_stop(&cfg.run_ctx, &Outcome::Completed(text))
                .await;
            send_stream_item(tx, Ok(event)).await;
        }
        Err(err) => {
            cfg.hooks
                .apply_on_stop(&cfg.run_ctx, &Outcome::Completed(text))
                .await;
            send_stream_item(tx, Err(err)).await;
        }
    }
}

async fn finish_error(
    cfg: &StreamCfg,
    err: KernelError,
    tx: &mpsc::Sender<Result<Event, KernelError>>,
) {
    // Kernel-side shutdown attribution: a run cancelled while the
    // kernel-wide shutdown token is fired gets `CancelReason::Shutdown`
    // stamped into the write-once cell so `on_stop` observers can tell
    // shutdown-driven cancellation from user or hook cancellation even
    // when the host wired no pause plumbing. First-write-wins keeps an
    // earlier `StopPattern` (user intent) ahead of the shutdown stamp.
    if matches!(err, KernelError::Cancelled) && cfg.shutdown.is_cancelled() {
        // First-write-wins: a rejected stamp means an earlier observer
        // (e.g. user StopPattern) already attributed the cancel.
        let _rejected = cfg.run_ctx.set_cancel_reason(CancelReason::Shutdown);
    }
    let outcome = outcome_for_error(&err);
    if matches!(outcome, Outcome::Errored(_)) {
        cfg.hooks.apply_on_error(&cfg.run_ctx, &err).await;
    }
    cfg.hooks.apply_on_stop(&cfg.run_ctx, &outcome).await;
    send_stream_item(tx, Err(err)).await;
}

async fn drive_inner(
    cfg: &StreamCfg,
    initial: Vec<Message>,
    cancel: Option<&CancellationToken>,
    tx: &mpsc::Sender<Result<Event, KernelError>>,
) -> Result<String, KernelError> {
    let chain = &cfg.hooks;
    chain.apply_on_session_start(&cfg.run_ctx).await?;
    let mut messages = MessageLog::from_initial(chain, &cfg.run_ctx, initial).await?;
    let tools = tool_defs(&cfg.tools);

    for _iter in 0..cfg.max_turns {
        // Turn-boundary pause point: the previous turn (including all of
        // its tool calls) has fully landed in the message log, the next
        // provider call has not started. A run that produced its final
        // text never reaches this check again, so a completed run always
        // wins over a pause request.
        check_pause(&cfg.pause)?;
        if let Some(text) = stream_one_turn(cfg, &mut messages, &tools, cancel, tx).await? {
            return Ok(text);
        }
    }
    Err(KernelError::MaxTurnsExceeded(cfg.max_turns))
}

async fn resolve_effective_model(cfg: &StreamCfg) -> Result<ResolvedModelWithSource, KernelError> {
    // Hook-published session override (set by `SessionPersistHook` in
    // `on_session_start`) wins over the request-level escape hatch.
    // Callers that pre-set `cfg.session_model_override` without wiring a
    // session-persisting hook still get the legacy behavior.
    let session_override = cfg
        .run_ctx
        .session_model_override()
        .or(cfg.session_model_override.as_ref());
    resolve_model_target_with_overrides(
        &cfg.models,
        session_override,
        cfg.global_override_store.as_ref(),
        cfg.explicit_model.as_ref(),
        &cfg.model,
    )
    .await
    .map_err(super::model_resolution::map_resolve_error)
}

async fn resolve_effective_effort(
    cfg: &StreamCfg,
    model: &ResolvedModelWithSource,
) -> Result<ResolvedEffort, KernelError> {
    if let Some(effort) = cfg.reasoning_effort {
        return Ok(ResolvedEffort {
            effort: Some(effort),
            source: EffortSource::Explicit,
        });
    }
    if let Some(effort) = cfg.run_ctx.session_reasoning_effort_override() {
        return Ok(ResolvedEffort {
            effort: Some(effort),
            source: EffortSource::SessionOverride,
        });
    }
    if let Some(effort) = cfg
        .global_reasoning_effort_override_store
        .get_global_reasoning_effort_override()
        .await
        .map_err(|err| KernelError::ReasoningEffortOverrideStore {
            reason: err.to_string(),
        })?
    {
        return Ok(ResolvedEffort {
            effort: Some(effort),
            source: EffortSource::GlobalOverride,
        });
    }
    Ok(ResolvedEffort {
        effort: model.info.caps.reasoning_effort,
        source: EffortSource::ModelDefault,
    })
}

const fn effort_for_request(effort: ResolvedEffort) -> Option<ReasoningEffort> {
    match effort.source {
        EffortSource::ModelDefault => None,
        EffortSource::Explicit | EffortSource::SessionOverride | EffortSource::GlobalOverride => {
            effort.effort
        }
    }
}

fn target_for_resolved(resolved: &ResolvedModelWithSource) -> ModelTarget {
    ModelTarget::new(resolved.info.provider.clone(), resolved.info.id.clone())
}

fn primary_target_for_request(configured: &ModelTarget, request_model: &ModelId) -> ModelTarget {
    if configured.model() == request_model {
        configured.clone()
    } else {
        ModelTarget::id(request_model.clone())
    }
}

async fn stream_tool_call(
    cfg: &StreamCfg,
    messages: &mut MessageLog,
    current_model: &ResolvedModelWithSource,
    current_effort: &ResolvedEffort,
    call: ToolCall,
    cancel: Option<&CancellationToken>,
    tx: &mpsc::Sender<Result<Event, KernelError>>,
) -> Result<(), KernelError> {
    check_cancel(cancel)?;
    // Tool-dispatch pause point: a multi-tool turn stops before starting
    // the next tool instead of running another long tool during a pause
    // window. Not-yet-dispatched calls stay dangling in the log; resume
    // paths repair them with synthetic interrupted tool results.
    check_pause(&cfg.pause)?;
    let chain = &cfg.hooks;
    let call = chain.apply_before_tool(&call, &cfg.run_ctx).await?;
    emit_event(
        chain,
        &cfg.run_ctx,
        Event::ToolCallStarted(call.clone()),
        tx,
    )
    .await?;

    let result = resolve_tool_call(
        cfg.policy.as_ref(),
        &cfg.tools,
        &call,
        &cfg.run_ctx,
        current_model,
        current_effort,
        cancel,
    )
    .await?;
    let result = chain.apply_after_tool(&call, &result, &cfg.run_ctx).await?;
    // Prompt-injection Layer 2: wrap every tool result in boundary tags
    // at this single LLM-history sink. `emit_completed` below keeps the
    // pre-wrap `result` for hook-event parity (see crate::sanitize).
    let raw = match &result.output {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let wrapped = if result.is_error {
        crate::sanitize::wrap_tool_error(&call.name, &raw)
    } else {
        crate::sanitize::wrap_tool_output(&call.name, &raw)
    };
    messages
        .append(
            chain,
            &cfg.run_ctx,
            Message::ToolResult {
                call_id: result.call_id.clone(),
                output: Value::String(wrapped),
                is_error: result.is_error,
            },
        )
        .await?;
    emit_completed(chain, &cfg.run_ctx, &call, &result, tx).await?;
    for message in result.run_messages.iter().cloned() {
        messages.append(chain, &cfg.run_ctx, message).await?;
    }
    Ok(())
}
