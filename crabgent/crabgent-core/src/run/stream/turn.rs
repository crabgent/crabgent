//! One streaming LLM turn: request preparation and response application.

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::action::Action;
use crate::error::KernelError;
use crate::hook::Event;
use crate::message::Message;
use crate::model::{ResolvedEffort, ResolvedModelWithSource};
use crate::types::{LlmRequest, LlmResponse, ToolDef};

use super::provider_turn::{ProviderTurn, consume_provider_stream};
use super::{
    MessageLog, StreamCfg, effort_for_request, primary_target_for_request,
    resolve_effective_effort, resolve_effective_model, stream_tool_call, target_for_resolved,
};
use crate::run::fallback::{
    FallbackEnv, attempt_kind_for_index, open_stream_with_fallbacks, preflight_attempt,
};
use crate::run::model_resolution::{ResolvedModel, resolve_attempts};
use crate::run::shared::{build_request, check_cancel, check_policy};

pub(super) async fn stream_one_turn(
    cfg: &StreamCfg,
    messages: &mut MessageLog,
    tools: &[ToolDef],
    cancel: Option<&CancellationToken>,
    tx: &mpsc::Sender<Result<Event, KernelError>>,
) -> Result<Option<String>, KernelError> {
    check_cancel(cancel)?;
    let surface = bind_run_surface(cfg, messages, tools).await?;
    check_cancel(cancel)?;

    let env = fallback_env(cfg, &surface, cancel, Some(tx));
    let opened = open_stream_with_fallbacks(&env).await?;
    let turn = consume_provider_stream(cfg, &surface.req, &surface.attempts, opened, tx).await?;
    let resp = cfg
        .hooks
        .apply_after_llm(&turn.request, &turn.response, &cfg.run_ctx)
        .await?;
    apply_provider_response(cfg, messages, turn, resp, cancel, tx).await
}

struct RunSurface {
    req: LlmRequest,
    attempts: Vec<ResolvedModel>,
    effective_model: ResolvedModelWithSource,
    effective_effort: ResolvedEffort,
}

async fn bind_run_surface(
    cfg: &StreamCfg,
    messages: &mut MessageLog,
    tools: &[ToolDef],
) -> Result<RunSurface, KernelError> {
    check_policy(cfg.policy.as_ref(), &cfg.run_ctx, &Action::LlmCall).await?;
    if let Some(compacted_log) = messages
        .compacted_for_provider(&cfg.hooks, &cfg.run_ctx)
        .await?
    {
        *messages = compacted_log;
    }
    let effective_model = resolve_effective_model(cfg).await?;
    let effective_effort = resolve_effective_effort(cfg, &effective_model).await?;
    let effective_target = target_for_resolved(&effective_model);
    let req = LlmRequest {
        web_search: cfg.web_search.clone(),
        ..build_request(
            effective_target.model(),
            cfg.system_prompt.as_deref(),
            messages.raw(),
            tools,
            cfg.max_tokens,
            cfg.temperature,
            effort_for_request(effective_effort),
        )
    };
    let req = cfg.hooks.apply_before_llm(&req, &cfg.run_ctx).await?;
    let primary = primary_target_for_request(&effective_target, &req.model);
    let attempts = resolve_attempts(&cfg.models, &primary, &cfg.fallbacks)?;
    let mut surface = RunSurface {
        req,
        attempts,
        effective_model,
        effective_effort,
    };
    prune_unflyable_fallbacks(cfg, &mut surface).await?;
    Ok(surface)
}

/// Pre-flight the resolved attempts and drop fallbacks that cannot serve this
/// request. The primary attempt stays fail-closed: an unsupported capability or
/// a denied hosted-web-search policy on the primary aborts the run as its real
/// error. A fallback that fails the same checks is removed from the chain so a
/// broken or request-incompatible fallback never aborts a healthy primary.
async fn prune_unflyable_fallbacks(
    cfg: &StreamCfg,
    surface: &mut RunSurface,
) -> Result<(), KernelError> {
    let mut survive = decide_attempt_survival(cfg, surface).await?.into_iter();
    surface.attempts.retain(|_| survive.next() == Some(true));
    Ok(())
}

/// Decide, per resolved attempt, whether it can serve the request. Returns a
/// survival mask aligned with `surface.attempts`. A primary (index 0)
/// pre-flight failure is returned as the fatal run error; a fallback failure
/// marks that attempt for removal.
async fn decide_attempt_survival(
    cfg: &StreamCfg,
    surface: &RunSurface,
) -> Result<Vec<bool>, KernelError> {
    let env = fallback_env(cfg, surface, None, None);
    let mut checked_web_search_providers: Vec<String> = Vec::new();
    let mut survive = Vec::with_capacity(surface.attempts.len());
    for (idx, attempt) in surface.attempts.iter().enumerate() {
        match preflight_one(cfg, &env, attempt, idx, &mut checked_web_search_providers).await {
            Ok(()) => survive.push(true),
            Err(err) if idx == 0 => return Err(err),
            Err(_) => survive.push(false),
        }
    }
    Ok(survive)
}

/// Capability plus hosted-web-search-policy pre-flight for one attempt.
async fn preflight_one(
    cfg: &StreamCfg,
    env: &FallbackEnv<'_>,
    attempt: &ResolvedModel,
    idx: usize,
    checked_web_search_providers: &mut Vec<String>,
) -> Result<(), KernelError> {
    let attempt_kind = attempt_kind_for_index(idx);
    let (provider, req) = preflight_attempt(env, attempt, attempt_kind)?;
    if req.web_search.enabled
        && !checked_web_search_providers
            .iter()
            .any(|name| name == provider.name())
    {
        check_policy(
            cfg.policy.as_ref(),
            &cfg.run_ctx,
            &Action::HostedWebSearch {
                provider: provider.name().to_owned(),
            },
        )
        .await?;
        checked_web_search_providers.push(provider.name().to_owned());
    }
    Ok(())
}

fn fallback_env<'a>(
    cfg: &'a StreamCfg,
    surface: &'a RunSurface,
    cancel: Option<&'a CancellationToken>,
    stream_tx: Option<&'a mpsc::Sender<Result<Event, KernelError>>>,
) -> FallbackEnv<'a> {
    FallbackEnv {
        providers: cfg.providers.as_ref(),
        registry: cfg.models.as_ref(),
        base: &surface.req,
        attempts: &surface.attempts,
        model_source: surface.effective_model.source,
        effort_source: surface.effective_effort.source,
        ctx: &cfg.run_ctx,
        cancel,
        hooks: &cfg.hooks,
        stream_tx,
    }
}

async fn apply_provider_response(
    cfg: &StreamCfg,
    messages: &mut MessageLog,
    turn: ProviderTurn,
    resp: LlmResponse,
    cancel: Option<&CancellationToken>,
    tx: &mpsc::Sender<Result<Event, KernelError>>,
) -> Result<Option<String>, KernelError> {
    let ProviderTurn {
        current_model,
        current_effort,
        server_tool_blocks,
        ..
    } = turn;

    // Append any server-tool-result blocks produced during this turn so the
    // next LLM turn can replay them for providers that require it.
    for block in server_tool_blocks {
        messages.append(&cfg.hooks, &cfg.run_ctx, block).await?;
    }

    if resp.tool_calls.is_empty() {
        let text = resp.text.clone();
        messages
            .append(
                &cfg.hooks,
                &cfg.run_ctx,
                Message::Assistant {
                    text: resp.text,
                    tool_calls: vec![],
                },
            )
            .await?;
        return Ok(Some(text));
    }

    messages
        .append(
            &cfg.hooks,
            &cfg.run_ctx,
            Message::Assistant {
                text: resp.text,
                tool_calls: resp.tool_calls.clone(),
            },
        )
        .await?;
    for call in resp.tool_calls {
        stream_tool_call(
            cfg,
            messages,
            &current_model,
            &current_effort,
            call,
            cancel,
            tx,
        )
        .await?;
    }
    Ok(None)
}
