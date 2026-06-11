//! Operation parsing and dispatch helpers for [`crate::MemoryTool`].

pub mod get_delete;
pub mod relations;
pub mod search;
pub mod store;

use chrono::{DateTime, Utc};
use crabgent_core::MemoryScope;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Search,
    Store,
    Get,
    Delete,
    RelationStore,
    RelationDelete,
    RelationExpand,
}

#[derive(Debug, Deserialize)]
pub struct Args {
    pub op: Op,
    pub scope: MemoryScope,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub importance: Option<f32>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub include_expired: Option<bool>,
    #[serde(default)]
    pub include_archived: Option<bool>,
    #[serde(default)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub from_id: Option<String>,
    #[serde(default)]
    pub to_id: Option<String>,
    #[serde(default)]
    pub relation_type: Option<String>,
    #[serde(default)]
    pub depth: Option<u32>,
}

#[cfg(test)]
pub mod test_support {
    use std::sync::Arc;

    use async_trait::async_trait;
    use crabgent_core::policy::AllowAllPolicy;
    use crabgent_core::{
        EmbeddingError, EmbeddingProvider, EmbeddingRequest, EmbeddingResponse, MemoryScope,
        ModelId, Owner, PolicyHook, RunCtx, Subject, ToolCtx,
    };
    use crabgent_store::{MemoryMemoryStore, MemoryStore};
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;

    use crate::MemoryTool;

    pub fn alice_ctx() -> ToolCtx {
        ToolCtx::new(Subject::new("alice"))
    }

    pub fn make_tool(policy: Arc<dyn PolicyHook>) -> (MemoryTool, Arc<MemoryMemoryStore>) {
        let store: Arc<MemoryMemoryStore> = Arc::new(MemoryMemoryStore::default());
        let store_dyn: Arc<dyn MemoryStore> = store.clone();
        (MemoryTool::new(store_dyn, policy, None), store)
    }

    pub fn make_tool_with_embedding_provider(
        policy: Arc<dyn PolicyHook>,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> (MemoryTool, Arc<MemoryMemoryStore>) {
        let store: Arc<MemoryMemoryStore> = Arc::new(MemoryMemoryStore::default());
        let store_dyn: Arc<dyn MemoryStore> = store.clone();
        (MemoryTool::new(store_dyn, policy, Some(provider)), store)
    }

    pub fn allow_all_tool() -> (MemoryTool, Arc<MemoryMemoryStore>) {
        make_tool(Arc::new(AllowAllPolicy))
    }

    pub fn alice_scope_value() -> Value {
        json!({"owner": "alice"})
    }

    pub fn alice_scope() -> MemoryScope {
        MemoryScope::for_owner(Owner::new("alice"))
    }

    pub struct FixedEmbeddingProvider {
        vector: Vec<f32>,
        model: ModelId,
    }

    impl FixedEmbeddingProvider {
        pub fn new(vector: Vec<f32>) -> Self {
            Self {
                vector,
                model: ModelId::new("test-embedding"),
            }
        }
    }

    #[async_trait]
    impl EmbeddingProvider for FixedEmbeddingProvider {
        fn dim(&self) -> usize {
            self.vector.len()
        }

        fn model_id(&self) -> &ModelId {
            &self.model
        }

        async fn embed(
            &self,
            req: EmbeddingRequest,
            _ctx: &RunCtx,
            _cancel: Option<&CancellationToken>,
        ) -> Result<EmbeddingResponse, EmbeddingError> {
            Ok(EmbeddingResponse {
                vectors: req.texts.iter().map(|_| self.vector.clone()).collect(),
                model: req.model.unwrap_or_else(|| self.model.clone()),
                dim: self.dim(),
                usage: None,
            })
        }
    }

    pub struct FailingEmbeddingProvider {
        error: EmbeddingError,
        model: ModelId,
    }

    impl FailingEmbeddingProvider {
        pub fn new(error: EmbeddingError) -> Self {
            Self {
                error,
                model: ModelId::new("test-embedding"),
            }
        }
    }

    #[async_trait]
    impl EmbeddingProvider for FailingEmbeddingProvider {
        fn dim(&self) -> usize {
            3
        }

        fn model_id(&self) -> &ModelId {
            &self.model
        }

        async fn embed(
            &self,
            _req: EmbeddingRequest,
            _ctx: &RunCtx,
            _cancel: Option<&CancellationToken>,
        ) -> Result<EmbeddingResponse, EmbeddingError> {
            Err(self.error.clone())
        }
    }
}
