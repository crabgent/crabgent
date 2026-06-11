//! Conflict resolution for near-duplicate memories.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{LlmRequest, ModelId, Provider, ProviderError, RunCtx, RunId, Subject};
use crabgent_log::warn;
use crabgent_store::MemoryDoc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::ConsolidationError;
use crate::extract::ExtractedFact;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictDecision {
    KeepExisting,
    Replace,
    BothValid,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictResolution {
    pub decision: ConflictDecision,
    pub reason: String,
}

impl ConflictResolution {
    pub fn new(decision: ConflictDecision, reason: impl Into<String>) -> Self {
        Self {
            decision,
            reason: reason.into(),
        }
    }
}

#[async_trait]
pub trait ConflictResolver: Send + Sync {
    async fn resolve(
        &self,
        existing: &MemoryDoc,
        fact: &ExtractedFact,
        token: &CancellationToken,
    ) -> Result<ConflictResolution, ConsolidationError>;
}

pub struct LlmConflictResolver {
    provider: Arc<dyn Provider>,
    model: ModelId,
    prompt: String,
}

impl LlmConflictResolver {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<ModelId>) -> Self {
        Self {
            provider,
            model: model.into(),
            prompt: "Resolve whether a new memory fact replaces, duplicates, or coexists with an existing memory.".to_owned(),
        }
    }

    #[must_use]
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = prompt.into();
        self
    }

    fn request(&self, existing: &MemoryDoc, fact: &ExtractedFact) -> LlmRequest {
        LlmRequest {
            model: self.model.clone(),
            system_prompt: Some(self.prompt.clone()),
            messages: vec![json!({
                "role": "user",
                "content": {
                    "existing": existing.body,
                    "new_fact": fact.content,
                }
            })],
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            stop_sequences: Vec::new(),
            reasoning_effort: None,
            web_search: crabgent_core::types::WebSearchConfig::default(),
            tool_choice: None,
        }
    }
}

#[async_trait]
impl ConflictResolver for LlmConflictResolver {
    async fn resolve(
        &self,
        existing: &MemoryDoc,
        fact: &ExtractedFact,
        token: &CancellationToken,
    ) -> Result<ConflictResolution, ConsolidationError> {
        let req = self.request(existing, fact);
        let ctx = RunCtx::new(RunId::new(), Subject::new("memory-consolidation"));
        let response = tokio::select! {
            result = self.provider.complete(&req, &ctx, Some(token)) => match result {
                Ok(response) => response,
                Err(err) => return Ok(provider_error_skip(&*self.provider, &err)),
            },
            () = token.cancelled() => return Err(ConsolidationError::Cancelled),
        };
        Ok(parse_resolution(&response.text))
    }
}

fn provider_error_skip(provider: &dyn Provider, err: &ProviderError) -> ConflictResolution {
    warn!(
        provider = provider.name(),
        error_kind = provider_error_kind(err),
        status = provider_error_status(err),
        retry_after_secs = provider_error_retry_after(err),
        "conflict resolver provider error; skipping resolution"
    );
    ConflictResolution::new(ConflictDecision::Skip, "provider error; skipped")
}

const fn provider_error_kind(err: &ProviderError) -> &'static str {
    match err {
        ProviderError::Transport(_) => "transport",
        ProviderError::Api { .. } => "api",
        ProviderError::Auth(_) => "auth",
        ProviderError::RateLimited { .. } => "rate_limited",
        ProviderError::MalformedResponse(_) => "malformed_response",
        ProviderError::ModelDiscovery { .. } => "model_discovery",
        ProviderError::ToolsUnsupported { .. } => "tools_unsupported",
        ProviderError::VisionUnsupported { .. } => "vision_unsupported",
        ProviderError::AudioUnsupported { .. } => "audio_unsupported",
        ProviderError::RetryableStream { .. } => "retryable_stream",
        ProviderError::Cancelled => "cancelled",
        ProviderError::Timeout => "timeout",
        ProviderError::Other(_) => "other",
        _ => "unknown",
    }
}

const fn provider_error_status(err: &ProviderError) -> Option<u16> {
    match err {
        ProviderError::Api { status, .. } => Some(*status),
        _ => None,
    }
}

const fn provider_error_retry_after(err: &ProviderError) -> Option<u64> {
    match err {
        ProviderError::Api {
            retry_after_secs, ..
        }
        | ProviderError::RateLimited { retry_after_secs } => *retry_after_secs,
        _ => None,
    }
}

fn parse_resolution(text: &str) -> ConflictResolution {
    let lower = text.to_ascii_lowercase();
    let decision = if lower.contains("replace") {
        ConflictDecision::Replace
    } else if lower.contains("both") {
        ConflictDecision::BothValid
    } else if lower.contains("keep") {
        ConflictDecision::KeepExisting
    } else {
        ConflictDecision::Skip
    };
    ConflictResolution::new(decision, text.trim())
}
