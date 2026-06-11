//! Provider fallback routing for one LLM call attempt chain.

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::{KernelError, ProviderError};
use crate::hook::{AttemptErrorClass, Event, RunCtx};
use crate::hook_chain::HookChain;
use crate::model::{
    EffortSource, ModelRegistry, ResolvedEffort, ResolvedModelWithSource, ResolvedSource,
};
use crate::provider::{EventStream, Provider};
use crate::provider_set::ProviderSet;
use crate::types::LlmRequest;

use super::model_resolution::{AttemptKind, ResolvedModel, request_for_attempt};
use super::shared::check_tool_advertise_limit;
use super::stream::events::emit_event;

const LONG_RATE_LIMIT_SECS: u64 = 30;

pub(super) struct OpenedStream {
    pub request: LlmRequest,
    pub stream: EventStream,
    pub attempt_idx: usize,
    pub current_model: ResolvedModelWithSource,
    pub current_effort: ResolvedEffort,
    pub resolved_effort_source: EffortSource,
}

/// Aggregate view of the inputs shared by every fallback attempt.
///
/// The kernel binds these together for the duration of one LLM call so the
/// fallback helpers do not need 6+ positional parameters each.
pub(super) struct FallbackEnv<'a> {
    pub providers: &'a ProviderSet,
    pub registry: &'a ModelRegistry,
    pub base: &'a LlmRequest,
    pub attempts: &'a [ResolvedModel],
    pub model_source: ResolvedSource,
    pub effort_source: EffortSource,
    pub ctx: &'a RunCtx,
    pub cancel: Option<&'a CancellationToken>,
    pub hooks: &'a HookChain,
    /// Stream channel for `Event::AttemptFailed` emission. `None` in the
    /// preflight-only path (`check_run_surface`) where no stream is yet
    /// open, so attempt diagnostics are suppressed.
    pub stream_tx: Option<&'a mpsc::Sender<Result<Event, KernelError>>>,
}

pub(super) async fn open_stream_with_fallbacks(
    env: &FallbackEnv<'_>,
) -> Result<OpenedStream, KernelError> {
    open_stream_from(env, 0).await
}

pub(super) async fn retry_pump_with_fallbacks(
    env: &FallbackEnv<'_>,
    failed_idx: usize,
    err: ProviderError,
) -> Result<OpenedStream, KernelError> {
    let will_fallback = should_try_next_pump(&err, failed_idx, env.attempts.len());
    let failing_attempt = env.attempts.get(failed_idx);
    let model = failing_attempt.map_or_else(
        || env.base.model.as_str(),
        |attempt| attempt.target.model().as_str(),
    );
    let provider = failing_attempt
        .and_then(|a| a.target.provider())
        .unwrap_or("unknown");
    emit_attempt_failed(env, failed_idx, provider, model, &err, will_fallback).await;
    if !will_fallback {
        return Err(err.into());
    }
    open_stream_from(env, failed_idx + 1).await
}

async fn open_stream_from(
    env: &FallbackEnv<'_>,
    start_idx: usize,
) -> Result<OpenedStream, KernelError> {
    for (idx, attempt) in env.attempts.iter().enumerate().skip(start_idx) {
        let attempt_kind = attempt_kind_for_index(idx);
        let (provider, req) = preflight_attempt(env, attempt, attempt_kind)?;
        match provider.stream(&req, env.ctx, env.cancel).await {
            Ok(stream) => {
                return Ok(OpenedStream {
                    current_model: ResolvedModelWithSource {
                        info: attempt.info.clone(),
                        source: env.model_source,
                    },
                    current_effort: current_effort_for_attempt(&req, attempt, env.effort_source),
                    resolved_effort_source: env.effort_source,
                    request: req,
                    stream,
                    attempt_idx: idx,
                });
            }
            Err(err) => {
                let will_fallback = should_try_next_open_stream(&err, idx, env.attempts.len());
                emit_attempt_failed(
                    env,
                    idx,
                    provider.name(),
                    req.model.as_str(),
                    &err,
                    will_fallback,
                )
                .await;
                if !will_fallback {
                    return Err(err.into());
                }
            }
        }
    }
    Err(KernelError::Internal("fallback chain exhausted".into()))
}

/// Push an `Event::AttemptFailed` through the hook chain and the streaming
/// channel. Short-circuits when the `FallbackEnv` has no `stream_tx`
/// (preflight path), in which case the kernel has not yet promised a stream
/// to any consumer and emission is a no-op.
///
/// Emission failures are discarded so the underlying `ProviderError` reaches
/// the caller unmasked. `HookDenied` on this informational event means a
/// hook in the chain refused to forward it, but earlier hooks (notably
/// `LogHook`) already observed and translated it before the denial fired;
/// the diagnostic signal is preserved in operator logs without taking over
/// the run's error class. Stream-receiver-closed means the consumer is
/// gone, in which case continuing the fallback chain to its natural error
/// or success is the right exit. This is the same silent-cleanup shape used
/// in `tool/bash.rs::drain_cancelled_output`.
async fn emit_attempt_failed(
    env: &FallbackEnv<'_>,
    attempt_idx: usize,
    provider: &str,
    model: &str,
    err: &ProviderError,
    will_fallback: bool,
) {
    let Some(tx) = env.stream_tx else {
        return;
    };
    let event = Event::AttemptFailed {
        attempt_idx,
        total_attempts: env.attempts.len(),
        provider: provider.to_owned(),
        model: model.to_owned(),
        error_class: AttemptErrorClass::from(err),
        message: err.to_string(),
        will_fallback,
    };
    // Informational event: see doc above for why emission failures are
    // discarded instead of propagated.
    if let Err(_err) = emit_event(env.hooks, env.ctx, event, tx).await {}
}

const fn current_effort_for_attempt(
    req: &LlmRequest,
    attempt: &ResolvedModel,
    resolved_source: EffortSource,
) -> ResolvedEffort {
    let source = if req.reasoning_effort.is_none() && attempt.info.caps.reasoning_effort.is_none() {
        EffortSource::ModelDefault
    } else {
        resolved_source
    };
    ResolvedEffort {
        effort: req.reasoning_effort,
        source,
    }
}

pub(super) fn preflight_attempt<'a>(
    env: &'a FallbackEnv<'_>,
    attempt: &ResolvedModel,
    attempt_kind: AttemptKind,
) -> Result<(&'a dyn Provider, LlmRequest), KernelError> {
    let provider_name = attempt
        .target
        .provider()
        .ok_or_else(|| KernelError::Internal("resolved model target is unqualified".into()))?;
    let provider = env.providers.provider_named(provider_name)?;
    let req = request_for_attempt(env.base, attempt, attempt_kind);
    check_tool_advertise_limit(provider.as_ref(), req.tools.len())?;
    super::tools_check::check_tools_capability(
        &req,
        &provider.capabilities(),
        &attempt.info.caps,
        provider.name(),
        req.model.as_str(),
    )?;
    super::vision_check::check_vision_capability(&req, provider.as_ref(), env.registry)?;
    super::audio_check::check_audio_capability(
        &req,
        &provider.capabilities(),
        &attempt.info.caps,
        provider.name(),
        req.model.as_str(),
    )?;
    super::web_search_check::check_web_search_capability(
        &req,
        &provider.capabilities(),
        &attempt.info.caps,
        provider.name(),
        req.model.as_str(),
    )?;
    check_reasoning_effort_capability(
        &req,
        &attempt.info.caps,
        provider.name(),
        req.model.as_str(),
    )?;
    Ok((provider.as_ref(), req))
}

pub(super) const fn attempt_kind_for_index(idx: usize) -> AttemptKind {
    if idx == 0 {
        AttemptKind::Primary
    } else {
        AttemptKind::Fallback
    }
}

const fn should_try_next_open_stream(err: &ProviderError, idx: usize, attempts_len: usize) -> bool {
    idx + 1 < attempts_len && should_fallback_open_stream(err)
}

const fn should_try_next_pump(err: &ProviderError, idx: usize, attempts_len: usize) -> bool {
    idx + 1 < attempts_len && should_fallback_pump(err)
}

const fn should_fallback_open_stream(err: &ProviderError) -> bool {
    match err {
        ProviderError::RateLimited { retry_after_secs } => {
            rate_limit_should_fallback(*retry_after_secs)
        }
        ProviderError::Api {
            status: 429,
            retry_after_secs,
            ..
        } => rate_limit_should_fallback(*retry_after_secs),
        ProviderError::Api { status, .. } => matches!(*status, 408 | 500..=599),
        ProviderError::RetryableStream { .. }
        | ProviderError::Transport(_)
        | ProviderError::Timeout
        | ProviderError::Other(_) => true,
        ProviderError::Auth(_)
        | ProviderError::MalformedResponse(_)
        | ProviderError::ModelDiscovery { .. }
        | ProviderError::ToolsUnsupported { .. }
        | ProviderError::VisionUnsupported { .. }
        | ProviderError::AudioUnsupported { .. }
        | ProviderError::WebSearchUnsupported { .. }
        | ProviderError::ReasoningEffortUnsupported { .. }
        | ProviderError::Cancelled => false,
    }
}

const fn should_fallback_pump(err: &ProviderError) -> bool {
    matches!(err, ProviderError::RetryableStream { .. })
}

const fn rate_limit_should_fallback(retry_after_secs: Option<u64>) -> bool {
    match retry_after_secs {
        Some(secs) => secs < LONG_RATE_LIMIT_SECS,
        None => true,
    }
}

fn check_reasoning_effort_capability(
    req: &LlmRequest,
    caps: &crate::model::ModelCapabilities,
    provider: &str,
    model: &str,
) -> Result<(), ProviderError> {
    if req.reasoning_effort.is_some() && caps.reasoning_effort.is_none() {
        return Err(ProviderError::ReasoningEffortUnsupported {
            provider: provider.to_owned(),
            model: model.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_rate_limit_falls_back() {
        let err = ProviderError::RateLimited {
            retry_after_secs: Some(5),
        };
        assert!(should_fallback_open_stream(&err));
    }

    #[test]
    fn thirty_second_rate_limit_does_not_fall_back() {
        let err = ProviderError::RateLimited {
            retry_after_secs: Some(30),
        };
        assert!(!should_fallback_open_stream(&err));
    }

    #[test]
    fn server_errors_fall_back() {
        let err = ProviderError::Api {
            status: 503,
            message: "busy".into(),
            retry_after_secs: None,
        };
        assert!(should_fallback_open_stream(&err));
    }

    #[test]
    fn fallback_429_with_long_retry_after_does_not_fall_back() {
        let err = ProviderError::Api {
            status: 429,
            message: "too many requests".into(),
            retry_after_secs: Some(60),
        };
        assert!(!should_fallback_open_stream(&err));
    }

    #[test]
    fn fallback_429_with_short_retry_after_falls_back() {
        let err = ProviderError::Api {
            status: 429,
            message: "too many requests".into(),
            retry_after_secs: Some(5),
        };
        assert!(should_fallback_open_stream(&err));
    }

    #[test]
    fn timeouts_fall_back() {
        assert!(should_fallback_open_stream(&ProviderError::Timeout));
    }

    #[test]
    fn retryable_stream_errors_fall_back_for_open_and_pump() {
        let err = ProviderError::RetryableStream {
            message: "overloaded_error: busy".into(),
        };
        assert!(should_fallback_open_stream(&err));
        assert!(should_fallback_pump(&err));
    }

    #[test]
    fn pump_fallback_rejects_plain_parser_errors() {
        assert!(!should_fallback_pump(&ProviderError::Other(
            "tool call input malformed".into()
        )));
    }

    #[test]
    fn auth_errors_do_not_fall_back() {
        assert!(!should_fallback_open_stream(&ProviderError::Auth(
            "bad key".into()
        )));
    }

    #[test]
    fn malformed_responses_do_not_fall_back() {
        assert!(!should_fallback_open_stream(
            &ProviderError::MalformedResponse("bad json".into())
        ));
    }

    #[test]
    fn model_discovery_errors_do_not_fall_back() {
        assert!(!should_fallback_open_stream(
            &ProviderError::ModelDiscovery {
                reason: "models endpoint unavailable".into(),
            }
        ));
    }

    #[test]
    fn vision_unsupported_does_not_fall_back() {
        assert!(!should_fallback_open_stream(
            &ProviderError::VisionUnsupported {
                provider: "anthropic".into(),
                model: "claude-3-haiku".into(),
            }
        ));
    }

    #[test]
    fn tools_unsupported_does_not_fall_back() {
        assert!(!should_fallback_open_stream(
            &ProviderError::ToolsUnsupported {
                provider: "openai".into(),
                model: "gpt-5.5".into(),
            }
        ));
    }

    #[test]
    fn audio_unsupported_does_not_fall_back() {
        assert!(!should_fallback_open_stream(
            &ProviderError::AudioUnsupported {
                provider: "openai".into(),
                model: "gpt-5.5".into(),
            }
        ));
    }
}
