//! Summary-provider fallback chain for semantic compaction.

use std::fmt;
use std::sync::Arc;

use crabgent_core::{
    ContentBlock, LlmRequest, Message, ModelId, ModelInfo, Provider, ProviderError, RawMessages,
    RunCtx,
};
use crabgent_log::{redact_uid, warn};
use crabgent_store::StoreError;

use crate::config::CompactConfig;

#[derive(Clone)]
pub struct SummaryAttempt {
    pub(crate) provider: Arc<dyn Provider>,
    pub(crate) model: ModelId,
}

impl SummaryAttempt {
    pub(crate) fn new<P>(provider: Arc<P>, model: impl Into<ModelId>) -> Self
    where
        P: Provider + 'static,
    {
        Self {
            provider,
            model: model.into(),
        }
    }

    fn provider_name(&self) -> &'static str {
        self.provider.name()
    }

    fn model_info(&self) -> Option<ModelInfo> {
        self.provider
            .models()
            .into_iter()
            .find(|info| model_matches(info, &self.model))
    }

    fn effective_summary_max_tokens(&self, config: &CompactConfig, ctx: &RunCtx) -> Option<u32> {
        let configured = config.summary_max_tokens?;
        let Some(info) = self.model_info() else {
            warn!(
                run_id = %ctx.run_id,
                subject = %redact_uid(ctx.subject.id()),
                provider = self.provider_name(),
                model = %self.model,
                configured_max_tokens = configured,
                "compact hook: summary model is not advertised; cannot clamp max_tokens",
            );
            return Some(configured);
        };
        Some(configured.min(info.caps.max_output_tokens))
    }

    fn effective_summary_temperature(&self, config: &CompactConfig, ctx: &RunCtx) -> Option<f32> {
        let configured = config.summary_temperature?;
        let Some(info) = self.model_info() else {
            warn!(
                run_id = %ctx.run_id,
                subject = %redact_uid(ctx.subject.id()),
                provider = self.provider_name(),
                model = %self.model,
                "compact hook: summary model is not advertised; forwarding configured temperature",
            );
            return Some(configured);
        };
        if info.caps.supports_temperature {
            Some(configured)
        } else {
            None
        }
    }
}

fn model_matches(info: &ModelInfo, model: &ModelId) -> bool {
    info.id == *model || info.aliases.iter().any(|alias| alias == model)
}

#[derive(Debug)]
pub enum CompactError {
    Provider(ProviderError),
    EmptySummary,
    Exhausted,
    Store(StoreError),
    Denied(String),
}

impl CompactError {
    pub(crate) const fn reason(&self) -> &'static str {
        match self {
            Self::Provider(_) => "compaction summary provider failed",
            Self::EmptySummary => "compaction summary was empty",
            Self::Exhausted => "compaction fallback chain exhausted",
            Self::Store(_) => "compaction session store failed",
            Self::Denied(_) => "compaction was denied",
        }
    }
}

impl fmt::Display for CompactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(error) => write!(f, "provider failed: {error}"),
            Self::EmptySummary => f.write_str("empty summary"),
            Self::Exhausted => f.write_str("fallback chain exhausted"),
            Self::Store(error) => write!(f, "store failed: {error}"),
            Self::Denied(reason) => write!(f, "denied: {reason}"),
        }
    }
}

const LONG_RATE_LIMIT_SECS: u64 = 30;

const fn should_try_next(err: &ProviderError, idx: usize, attempts_len: usize) -> bool {
    has_next(idx, attempts_len) && should_fallback_summary(err)
}

const fn has_next(idx: usize, attempts_len: usize) -> bool {
    idx + 1 < attempts_len
}

const fn should_fallback_summary(err: &ProviderError) -> bool {
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
        _ => false,
    }
}

const fn rate_limit_should_fallback(retry_after_secs: Option<u64>) -> bool {
    match retry_after_secs {
        Some(secs) => secs < LONG_RATE_LIMIT_SECS,
        None => true,
    }
}

fn log_provider_fallback(
    ctx: &RunCtx,
    error: &ProviderError,
    failed: &SummaryAttempt,
    next: &SummaryAttempt,
) {
    warn!(
        run_id = %ctx.run_id,
        subject = %redact_uid(ctx.subject.id()),
        provider = failed.provider_name(),
        model = %failed.model,
        next_provider = next.provider_name(),
        next_model = %next.model,
        error = %error,
        "compact hook: summary provider failed; falling back",
    );
}

fn log_empty_fallback(ctx: &RunCtx, failed: &SummaryAttempt, next: &SummaryAttempt) {
    warn!(
        run_id = %ctx.run_id,
        subject = %redact_uid(ctx.subject.id()),
        provider = failed.provider_name(),
        model = %failed.model,
        next_provider = next.provider_name(),
        next_model = %next.model,
        "compact hook: summary provider returned empty text; falling back",
    );
}

pub async fn summarize_with_chain(
    attempts: &[SummaryAttempt],
    config: &CompactConfig,
    transcript: &str,
    prior_summary: Option<&str>,
    ctx: &RunCtx,
) -> Result<String, CompactError> {
    for (idx, attempt) in attempts.iter().enumerate() {
        let max_tokens = attempt.effective_summary_max_tokens(config, ctx);
        let temperature = attempt.effective_summary_temperature(config, ctx);
        let req = build_summary_request(
            config,
            &attempt.model,
            max_tokens,
            temperature,
            transcript,
            prior_summary,
        );
        match attempt.provider.complete(&req, ctx, None).await {
            Ok(resp) => {
                let summary = resp.text.trim();
                if summary.is_empty()
                    && let Some(next_attempt) = attempts.get(idx + 1)
                {
                    log_empty_fallback(ctx, attempt, next_attempt);
                    continue;
                }
                if summary.is_empty() {
                    return Err(CompactError::EmptySummary);
                }
                return Ok(summary.to_owned());
            }
            Err(error) if should_try_next(&error, idx, attempts.len()) => {
                if let Some(next_attempt) = attempts.get(idx + 1) {
                    log_provider_fallback(ctx, &error, attempt, next_attempt);
                }
            }
            Err(error) => return Err(CompactError::Provider(error)),
        }
    }
    Err(CompactError::Exhausted)
}

pub fn build_summary_request(
    config: &CompactConfig,
    model: &ModelId,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    transcript: &str,
    prior_summary: Option<&str>,
) -> LlmRequest {
    let text = match prior_summary {
        Some(prior_summary) => format!(
            "{}\n\n<prior_summary>\n{prior_summary}\n</prior_summary>\n\n\
             <transcript>\n{transcript}\n</transcript>\n\n{}",
            config.prior_summary_instruction, config.instruction
        ),
        None => format!(
            "<transcript>\n{transcript}\n</transcript>\n\n{}",
            config.instruction
        ),
    };
    let messages = RawMessages::from(vec![Message::User {
        content: vec![ContentBlock::Text { text }],
        timestamp: None,
    }])
    .into_inner();

    LlmRequest {
        model: model.clone(),
        system_prompt: Some(config.system_prompt.clone()),
        messages,
        tools: Vec::new(),
        max_tokens,
        temperature,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::{RunId, Subject};
    use crabgent_test_support::StubProvider;

    fn make_info(id: &str, supports_temperature: bool) -> ModelInfo {
        let mut info = ModelInfo::minimal(id, "static");
        info.caps.supports_temperature = supports_temperature;
        info
    }

    fn test_ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("test"))
    }

    fn attempt(infos: Vec<ModelInfo>, model: &str) -> SummaryAttempt {
        SummaryAttempt::new(
            Arc::new(StubProvider::new().with_models(infos)),
            ModelId::new(model),
        )
    }

    #[test]
    fn build_summary_request_uses_effective_max_tokens() {
        let config = CompactConfig {
            summary_max_tokens: Some(32_768),
            ..CompactConfig::default()
        };

        let req = build_summary_request(
            &config,
            &ModelId::new("summary"),
            Some(4_000),
            Some(0.0),
            "transcript",
            None,
        );

        assert_eq!(req.max_tokens, Some(4_000));
    }

    #[test]
    fn build_summary_request_forwards_temperature_some() {
        let config = CompactConfig::default();

        let req = build_summary_request(
            &config,
            &ModelId::new("summary"),
            None,
            Some(0.7),
            "transcript",
            None,
        );

        assert_eq!(req.temperature, Some(0.7));
    }

    #[test]
    fn build_summary_request_forwards_temperature_none() {
        let config = CompactConfig::default();

        let req = build_summary_request(
            &config,
            &ModelId::new("summary"),
            None,
            None,
            "transcript",
            None,
        );

        assert!(req.temperature.is_none());
    }

    #[test]
    fn effective_summary_temperature_keeps_when_supported() {
        let attempt = attempt(vec![make_info("summary", true)], "summary");
        let config = CompactConfig {
            summary_temperature: Some(0.0),
            ..CompactConfig::default()
        };

        let temperature = attempt.effective_summary_temperature(&config, &test_ctx());

        assert_eq!(temperature, Some(0.0));
    }

    #[test]
    fn effective_summary_temperature_drops_when_unsupported() {
        let attempt = attempt(vec![make_info("opus-4-7", false)], "opus-4-7");
        let config = CompactConfig {
            summary_temperature: Some(0.0),
            ..CompactConfig::default()
        };

        let temperature = attempt.effective_summary_temperature(&config, &test_ctx());

        assert!(
            temperature.is_none(),
            "temperature must be omitted for models that reject the field",
        );
    }

    #[test]
    fn effective_summary_temperature_fails_open_on_unknown_model() {
        let attempt = attempt(vec![make_info("other-model", true)], "missing-model");
        let config = CompactConfig {
            summary_temperature: Some(0.4),
            ..CompactConfig::default()
        };

        let temperature = attempt.effective_summary_temperature(&config, &test_ctx());

        assert_eq!(
            temperature,
            Some(0.4),
            "unknown model forwards the configured temperature with a warn",
        );
    }

    #[test]
    fn effective_summary_temperature_none_when_config_none() {
        let attempt = attempt(vec![make_info("summary", true)], "summary");
        let config = CompactConfig {
            summary_temperature: None,
            ..CompactConfig::default()
        };

        let temperature = attempt.effective_summary_temperature(&config, &test_ctx());

        assert!(temperature.is_none());
    }
}
