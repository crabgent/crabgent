//! Fact extraction from episodic memory docs.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{LlmRequest, ModelId, Provider, RunCtx, RunId, Subject};
use crabgent_store::MemoryDoc;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::ConsolidationError;

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedFact {
    pub content: String,
    pub kind: String,
    pub importance: f32,
    pub confidence: f32,
}

impl ExtractedFact {
    pub fn new(
        content: impl Into<String>,
        kind: impl Into<String>,
        importance: f32,
        confidence: f32,
    ) -> Self {
        Self {
            content: content.into(),
            kind: kind.into(),
            importance: importance.clamp(0.3, 0.9),
            confidence,
        }
    }
}

#[async_trait]
pub trait FactExtractor: Send + Sync {
    async fn extract(
        &self,
        doc: &MemoryDoc,
        token: &CancellationToken,
    ) -> Result<Vec<ExtractedFact>, ConsolidationError>;
}

pub struct LlmFactExtractor {
    provider: Arc<dyn Provider>,
    model: ModelId,
    prompt: String,
}

impl LlmFactExtractor {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<ModelId>) -> Self {
        Self {
            provider,
            model: model.into(),
            prompt: "Extract durable semantic facts from the episodic memory as JSON.".to_owned(),
        }
    }

    #[must_use]
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = prompt.into();
        self
    }

    fn request(&self, doc: &MemoryDoc) -> LlmRequest {
        LlmRequest {
            model: self.model.clone(),
            system_prompt: Some(self.prompt.clone()),
            messages: vec![json!({
                "role": "user",
                "content": doc.body,
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
impl FactExtractor for LlmFactExtractor {
    async fn extract(
        &self,
        doc: &MemoryDoc,
        token: &CancellationToken,
    ) -> Result<Vec<ExtractedFact>, ConsolidationError> {
        let req = self.request(doc);
        let ctx = RunCtx::new(RunId::new(), Subject::new("memory-consolidation"));
        let response = tokio::select! {
            result = self.provider.complete(&req, &ctx, Some(token)) => result?,
            () = token.cancelled() => return Err(ConsolidationError::Cancelled),
        };
        Ok(parse_facts(&response.text))
    }
}

#[derive(Deserialize)]
struct FactWire {
    content: String,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default = "default_importance")]
    importance: f32,
    #[serde(default = "default_confidence")]
    confidence: f32,
}

fn parse_facts(text: &str) -> Vec<ExtractedFact> {
    if let Ok(items) = serde_json::from_str::<Vec<FactWire>>(text) {
        return items
            .into_iter()
            .map(|item| {
                ExtractedFact::new(item.content, item.kind, item.importance, item.confidence)
            })
            .collect();
    }

    let trimmed = text.trim();
    if trimmed.is_empty() {
        Vec::new()
    } else {
        vec![ExtractedFact::new(
            trimmed.to_owned(),
            default_kind(),
            default_importance(),
            default_confidence(),
        )]
    }
}

fn default_kind() -> String {
    "semantic".to_owned()
}

const fn default_importance() -> f32 {
    0.5
}

const fn default_confidence() -> f32 {
    1.0
}
