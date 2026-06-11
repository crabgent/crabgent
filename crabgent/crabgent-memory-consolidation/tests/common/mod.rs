#![allow(
    dead_code,
    reason = "shared integration-test helpers are used per test target"
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::{
    LlmRequest, LlmResponse, MemoryScope, ModelInfo, Owner, Provider, ProviderCapabilities,
    ProviderError, RunCtx, StopReason, Subject, Usage,
};
use crabgent_memory::MemoryClass;
use crabgent_memory_consolidation::{
    ConflictDecision, ConflictResolution, ConflictResolver, ConsolidationConfig,
    ConsolidationCronExecutor, ConsolidationError, ConsolidationRunner, Deduplicator,
    ExtractedFact, FactExtractor, StaleCleaner, SubjectResolver,
};
use crabgent_store::{MemoryDoc, MemoryMemoryStore, MemoryStore};
use tokio_util::sync::CancellationToken;

pub fn scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new("alice"))
}

pub fn subject() -> Subject {
    Subject::new("alice")
}

pub fn long_body(label: &str) -> String {
    format!(
        "{label} has enough detail to pass the eighty character episodic body filter for consolidation tests."
    )
}

pub fn episodic_doc(body: impl Into<String>) -> MemoryDoc {
    let mut doc = MemoryDoc::new(scope(), body);
    doc.class = Some(MemoryClass::Episodic.as_str().to_owned());
    doc
}

pub fn semantic_doc(body: impl Into<String>) -> MemoryDoc {
    let mut doc = MemoryDoc::new(scope(), body);
    doc.class = Some(MemoryClass::Semantic.as_str().to_owned());
    doc
}

pub async fn store_doc(store: &MemoryMemoryStore, doc: &MemoryDoc) {
    store.store(doc).await.expect("store doc");
}

pub fn fact(content: impl Into<String>) -> ExtractedFact {
    ExtractedFact::new(content, MemoryClass::Semantic.as_str(), 0.6, 1.0)
}

pub fn token() -> CancellationToken {
    CancellationToken::new()
}

pub struct StaticFactExtractor {
    facts: Vec<ExtractedFact>,
}

impl StaticFactExtractor {
    pub const fn new(facts: Vec<ExtractedFact>) -> Self {
        Self { facts }
    }
}

#[async_trait]
impl FactExtractor for StaticFactExtractor {
    async fn extract(
        &self,
        _doc: &MemoryDoc,
        _token: &CancellationToken,
    ) -> Result<Vec<ExtractedFact>, ConsolidationError> {
        Ok(self.facts.clone())
    }
}

pub struct StaticConflictResolver {
    resolution: ConflictResolution,
}

impl StaticConflictResolver {
    pub fn new(decision: ConflictDecision) -> Self {
        Self {
            resolution: ConflictResolution::new(decision, "test resolution"),
        }
    }
}

#[async_trait]
impl ConflictResolver for StaticConflictResolver {
    async fn resolve(
        &self,
        _existing: &MemoryDoc,
        _fact: &ExtractedFact,
        _token: &CancellationToken,
    ) -> Result<ConflictResolution, ConsolidationError> {
        Ok(self.resolution.clone())
    }
}

pub fn runner_with(
    store: Arc<MemoryMemoryStore>,
    facts: Vec<ExtractedFact>,
    decision: ConflictDecision,
) -> ConsolidationRunner {
    let store_dyn: Arc<dyn MemoryStore> = store;
    let config = ConsolidationConfig::default();
    ConsolidationRunner::new(
        store_dyn.clone(),
        Arc::new(StaticFactExtractor::new(facts)),
        Deduplicator::new(store_dyn.clone()),
        Arc::new(StaticConflictResolver::new(decision)),
        StaleCleaner::new(store_dyn.clone(), config.stale_policy.clone()),
        Arc::new(crabgent_core::AllowAllPolicy),
        config,
    )
}

pub fn denying_runner(store: Arc<MemoryMemoryStore>) -> ConsolidationRunner {
    let store_dyn: Arc<dyn MemoryStore> = store;
    let config = ConsolidationConfig::default();
    ConsolidationRunner::new(
        store_dyn.clone(),
        Arc::new(StaticFactExtractor::new(Vec::new())),
        Deduplicator::new(store_dyn.clone()),
        Arc::new(StaticConflictResolver::new(ConflictDecision::Skip)),
        StaleCleaner::new(store_dyn, config.stale_policy.clone()),
        Arc::new(crabgent_core::DenyAllPolicy),
        config,
    )
}

pub enum MockProviderResponse {
    Text(String),
    Error,
}

#[derive(Default)]
pub struct MockProvider {
    responses: Mutex<VecDeque<MockProviderResponse>>,
}

impl MockProvider {
    pub fn fact_extract_returns(text: impl Into<String>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from([MockProviderResponse::Text(text.into())])),
        }
    }

    pub fn conflict_resolution_returns(text: impl Into<String>) -> Self {
        Self::fact_extract_returns(text)
    }

    pub fn provider_error_returns() -> Self {
        Self {
            responses: Mutex::new(VecDeque::from([MockProviderResponse::Error])),
        }
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let response = self.responses.lock().expect("provider mutex").pop_front();
        match response {
            Some(MockProviderResponse::Text(text)) => Ok(LlmResponse {
                text,
                tool_calls: Vec::new(),
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: req.model.clone(),
            }),
            Some(MockProviderResponse::Error) => Err(ProviderError::Other(
                "mock provider error with hidden details".to_owned(),
            )),
            None => Ok(LlmResponse {
                text: String::new(),
                tool_calls: Vec::new(),
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: req.model.clone(),
            }),
        }
    }

    fn name(&self) -> &'static str {
        "mock"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("mock-model", "mock")]
    }
}

pub fn mock_subject_resolver() -> SubjectResolver {
    Arc::new(|_job| Ok(subject()))
}

#[expect(
    dead_code,
    reason = "shared integration-test helper is used only by cron-facing consolidation tests"
)]
pub fn cron_executor(runner: Arc<ConsolidationRunner>) -> ConsolidationCronExecutor {
    ConsolidationCronExecutor::new(runner, mock_subject_resolver())
}
