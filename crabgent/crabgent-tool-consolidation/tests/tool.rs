use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::tool::{Tool, ToolCtx};
use crabgent_core::{
    MemoryScope, Owner, PolicyDecision, PolicyHook, SearchQuery, Subject, policy::AllowAllPolicy,
    policy::DenyAllPolicy,
};
use crabgent_memory_consolidation::{
    CLASS_CONSOLIDATION_CHECKPOINT, ConflictDecision, ConflictResolution, ConflictResolver,
    ConsolidationCheckpoint, ConsolidationConfig, ConsolidationError, ConsolidationRunner,
    Deduplicator, ExtractedFact, FactExtractor, StaleCleaner, StalePolicy,
};
use crabgent_store::{MemoryDoc, MemoryHit, MemoryMemoryStore, MemoryStore, StoreError};
use crabgent_tool_consolidation::ConsolidationTool;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct StaticFactExtractor {
    facts: Vec<ExtractedFact>,
}

impl StaticFactExtractor {
    const fn new(facts: Vec<ExtractedFact>) -> Self {
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

struct StaticConflictResolver {
    decision: ConflictDecision,
    message: &'static str,
}

impl StaticConflictResolver {
    const fn new(decision: ConflictDecision, message: &'static str) -> Self {
        Self { decision, message }
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
        Ok(ConflictResolution::new(self.decision, self.message))
    }
}

fn make_tool(
    policy: Arc<dyn PolicyHook>,
    facts: Vec<ExtractedFact>,
    decision: ConflictDecision,
    message: &'static str,
) -> (ConsolidationTool, Arc<MemoryMemoryStore>) {
    let store = Arc::new(MemoryMemoryStore::default());
    let memory_store: Arc<dyn MemoryStore> = store.clone();
    let runner = Arc::new(ConsolidationRunner::new(
        memory_store.clone(),
        Arc::new(StaticFactExtractor::new(facts)),
        Deduplicator::new(memory_store.clone()),
        Arc::new(StaticConflictResolver::new(decision, message)),
        StaleCleaner::new(memory_store.clone(), StalePolicy::default()),
        policy,
        ConsolidationConfig::default(),
    ));
    (ConsolidationTool::new(runner), store)
}

fn fact(body: &str) -> ExtractedFact {
    ExtractedFact::new(body, "semantic", 0.6, 1.0)
}

fn scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new("alice"))
}

fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("alice"))
}

fn run_args() -> Value {
    json!({
        "op": "run",
        "scope": {"owner": "alice"}
    })
}

fn status_args() -> Value {
    json!({
        "op": "status",
        "scope": {"owner": "alice"}
    })
}

fn episodic_doc(body: &str) -> MemoryDoc {
    let mut doc = MemoryDoc::new(scope(), format!("{body} {}", "detail ".repeat(20)));
    doc.class = Some("episodic".to_owned());
    doc.importance = Some(0.9);
    doc
}

fn semantic_doc(body: &str) -> MemoryDoc {
    let mut doc = MemoryDoc::new(scope(), body);
    doc.class = Some("semantic".to_owned());
    doc.importance = Some(0.8);
    doc
}

async fn store_doc(store: &Arc<MemoryMemoryStore>, doc: &MemoryDoc) {
    store.store(doc).await.expect("store doc");
}

struct FailingMemoryStore {
    message: &'static str,
}

impl FailingMemoryStore {
    const fn new(message: &'static str) -> Self {
        Self { message }
    }

    fn err(&self) -> StoreError {
        StoreError::backend(self.message)
    }
}

#[async_trait]
impl MemoryStore for FailingMemoryStore {
    async fn search(&self, _query: &SearchQuery) -> Result<Vec<MemoryHit>, StoreError> {
        Err(self.err())
    }

    async fn store(&self, _doc: &MemoryDoc) -> Result<crabgent_core::MemoryId, StoreError> {
        Err(self.err())
    }

    async fn get(&self, _id: &crabgent_core::MemoryId) -> Result<Option<MemoryDoc>, StoreError> {
        Err(self.err())
    }

    async fn delete(&self, _id: &crabgent_core::MemoryId) -> Result<bool, StoreError> {
        Err(self.err())
    }

    async fn delete_scoped(
        &self,
        _id: &crabgent_core::MemoryId,
        _scope: &MemoryScope,
    ) -> Result<bool, StoreError> {
        Err(self.err())
    }

    async fn update_body(
        &self,
        _id: &crabgent_core::MemoryId,
        _new_body: String,
    ) -> Result<bool, StoreError> {
        Err(self.err())
    }

    async fn update_body_with_embedding(
        &self,
        _id: &crabgent_core::MemoryId,
        _new_body: String,
        _embedding: Option<Vec<f32>>,
    ) -> Result<bool, StoreError> {
        Err(self.err())
    }
}

#[derive(Default)]
struct CountingMemoryStore {
    inner: Arc<MemoryMemoryStore>,
    search_calls: AtomicUsize,
    get_calls: AtomicUsize,
}

impl CountingMemoryStore {
    fn new(inner: Arc<MemoryMemoryStore>) -> Self {
        Self {
            inner,
            ..Self::default()
        }
    }

    fn search_calls(&self) -> usize {
        self.search_calls.load(Ordering::Relaxed)
    }

    fn get_calls(&self) -> usize {
        self.get_calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl MemoryStore for CountingMemoryStore {
    async fn search(&self, query: &SearchQuery) -> Result<Vec<MemoryHit>, StoreError> {
        self.search_calls.fetch_add(1, Ordering::Relaxed);
        self.inner.search(query).await
    }

    async fn store(&self, doc: &MemoryDoc) -> Result<crabgent_core::MemoryId, StoreError> {
        self.inner.store(doc).await
    }

    async fn get(&self, id: &crabgent_core::MemoryId) -> Result<Option<MemoryDoc>, StoreError> {
        self.get_calls.fetch_add(1, Ordering::Relaxed);
        self.inner.get(id).await
    }

    async fn delete(&self, id: &crabgent_core::MemoryId) -> Result<bool, StoreError> {
        self.inner.delete(id).await
    }

    async fn delete_scoped(
        &self,
        id: &crabgent_core::MemoryId,
        scope: &MemoryScope,
    ) -> Result<bool, StoreError> {
        self.inner.delete_scoped(id, scope).await
    }

    async fn update_body(
        &self,
        id: &crabgent_core::MemoryId,
        new_body: String,
    ) -> Result<bool, StoreError> {
        self.inner.update_body(id, new_body).await
    }

    async fn update_body_with_embedding(
        &self,
        id: &crabgent_core::MemoryId,
        new_body: String,
        embedding: Option<Vec<f32>>,
    ) -> Result<bool, StoreError> {
        self.inner
            .update_body_with_embedding(id, new_body, embedding)
            .await
    }
}

#[derive(Default)]
struct CountingPolicy {
    calls: AtomicUsize,
}

#[async_trait]
impl PolicyHook for CountingPolicy {
    async fn allow(&self, _subject: &Subject, _action: &crabgent_core::Action) -> PolicyDecision {
        self.calls.fetch_add(1, Ordering::Relaxed);
        PolicyDecision::Allow
    }
}

#[tokio::test]
async fn tool_run_dispatches_starts_pipeline() {
    let (tool, store) = make_tool(
        Arc::new(AllowAllPolicy),
        vec![fact("durable semantic preference")],
        ConflictDecision::Skip,
        "unused",
    );
    store_doc(&store, &episodic_doc("episodic source")).await;

    let output = tool.execute(run_args(), &ctx()).await.expect("test result");

    assert_eq!(tool.name(), "consolidate_memory");
    assert_eq!(output["stats"]["sessions_processed"], json!(1));
    assert_eq!(output["stats"]["facts_extracted"], json!(1));
    assert_eq!(output["stats"]["memories_created"], json!(1));
    assert!(output.get("run_id").is_none());
}

#[tokio::test]
async fn tool_status_dispatches_reads_checkpoint() {
    let (tool, store) = make_tool(
        Arc::new(AllowAllPolicy),
        vec![fact("status must not run pipeline")],
        ConflictDecision::Skip,
        "unused",
    );
    let checkpoint = ConsolidationCheckpoint {
        in_progress: true,
        sessions_processed: 7,
        ..ConsolidationCheckpoint::default()
    };
    let mut checkpoint_doc = MemoryDoc::new(
        scope(),
        serde_json::to_string(&checkpoint).expect("test result"),
    );
    checkpoint_doc.class = Some(CLASS_CONSOLIDATION_CHECKPOINT.to_owned());
    store_doc(&store, &checkpoint_doc).await;
    store_doc(&store, &episodic_doc("status-only episodic source")).await;

    let output = tool
        .execute(status_args(), &ctx())
        .await
        .expect("test result");
    let semantic_hits = store
        .search(&SearchQuery::new("").scope(scope()).class("semantic"))
        .await
        .expect("test result");

    assert_eq!(output["has_checkpoint"], json!(true));
    assert_eq!(output["in_progress"], json!(true));
    assert_eq!(output["sessions_processed_total"], json!(7));
    assert!(semantic_hits.is_empty());
}

#[tokio::test]
async fn tool_run_policy_deny_returns_permission_error() {
    let (tool, store) = make_tool(
        Arc::new(DenyAllPolicy),
        vec![fact("blocked semantic fact")],
        ConflictDecision::Skip,
        "unused",
    );
    store_doc(&store, &episodic_doc("blocked episodic source")).await;

    let err = tool
        .execute(run_args(), &ctx())
        .await
        .expect_err("expected error");

    assert!(matches!(err, ToolError::Permission(ref msg) if msg.contains("DenyAllPolicy")));
}

#[tokio::test]
async fn tool_status_policy_deny_returns_permission_error_without_checkpoint_read() {
    let inner = Arc::new(MemoryMemoryStore::default());
    let counting_store = Arc::new(CountingMemoryStore::new(inner.clone()));
    let memory_store: Arc<dyn MemoryStore> = counting_store.clone();
    let runner = Arc::new(ConsolidationRunner::new(
        memory_store.clone(),
        Arc::new(StaticFactExtractor::new(Vec::new())),
        Deduplicator::new(memory_store.clone()),
        Arc::new(StaticConflictResolver::new(
            ConflictDecision::Skip,
            "unused",
        )),
        StaleCleaner::new(memory_store.clone(), StalePolicy::default()),
        Arc::new(DenyAllPolicy),
        ConsolidationConfig::default(),
    ));
    let tool = ConsolidationTool::new(runner);
    let marker = "checkpoint-marker-must-not-leak";
    let checkpoint = ConsolidationCheckpoint {
        in_progress: true,
        sessions_processed: 11,
        ..ConsolidationCheckpoint::default()
    };
    let mut checkpoint_doc = MemoryDoc::new(
        scope(),
        format!(
            "{} {marker}",
            serde_json::to_string(&checkpoint).expect("serialize checkpoint")
        ),
    );
    checkpoint_doc.class = Some(CLASS_CONSOLIDATION_CHECKPOINT.to_owned());
    store_doc(&inner, &checkpoint_doc).await;

    let err = tool
        .execute(status_args(), &ctx())
        .await
        .expect_err("expected error");

    assert!(matches!(err, ToolError::Permission(ref msg) if msg.contains("DenyAllPolicy")));
    assert!(!err.to_string().contains(marker));
    assert_eq!(counting_store.search_calls(), 0);
    assert_eq!(counting_store.get_calls(), 0);
}

#[tokio::test]
async fn tool_run_policy_is_checked_once_by_runner() {
    let policy = Arc::new(CountingPolicy::default());
    let (tool, store) = make_tool(
        policy.clone(),
        vec![fact("single policy gate fact")],
        ConflictDecision::Skip,
        "unused",
    );
    store_doc(&store, &episodic_doc("policy counted episodic source")).await;

    tool.execute(run_args(), &ctx()).await.expect("test result");

    assert_eq!(policy.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn tool_run_output_no_reasoning_leak() {
    let marker = "SECRET-REASON-MARKER";
    let (tool, store) = make_tool(
        Arc::new(AllowAllPolicy),
        vec![fact("alpha beta gamma delta")],
        ConflictDecision::KeepExisting,
        marker,
    );
    store_doc(&store, &semantic_doc("alpha beta gamma delta epsilon")).await;
    store_doc(&store, &episodic_doc("conflicting episodic source")).await;

    let result = tool
        .execute_result(run_args(), &ctx())
        .await
        .expect("test result");
    let output = serde_json::to_string(&result.output).expect("test result");

    assert_eq!(result.output["stats"]["conflicts_detected"], json!(1));
    assert!(result.output["stats"].get("conflicts_resolved").is_none());
    assert!(!output.contains(marker));
}

#[tokio::test]
async fn tool_error_does_not_leak_store_dsn() {
    let marker = "secret-dsn-marker-12345";
    let store: Arc<dyn MemoryStore> = Arc::new(FailingMemoryStore::new(marker));
    let policy: Arc<dyn PolicyHook> = Arc::new(AllowAllPolicy);
    let runner = Arc::new(ConsolidationRunner::new(
        store.clone(),
        Arc::new(StaticFactExtractor::new(vec![fact("unreachable")])),
        Deduplicator::new(store.clone()),
        Arc::new(StaticConflictResolver::new(
            ConflictDecision::Skip,
            "unused",
        )),
        StaleCleaner::new(store.clone(), StalePolicy::default()),
        policy,
        ConsolidationConfig::default(),
    ));
    let tool = ConsolidationTool::new(runner);

    let run_err = tool
        .execute(run_args(), &ctx())
        .await
        .expect_err("expected error");
    let status_err = tool
        .execute(status_args(), &ctx())
        .await
        .expect_err("expected error");

    assert!(!run_err.to_string().contains(marker));
    assert!(
        run_err
            .to_string()
            .contains("consolidation: store unavailable")
    );
    assert!(!status_err.to_string().contains(marker));
    assert!(
        status_err
            .to_string()
            .contains("consolidation: store unavailable")
    );
}
