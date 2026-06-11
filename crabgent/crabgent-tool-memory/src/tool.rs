//! [`MemoryTool`] implementation.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::tool::{Tool, ToolCtx, gate_tool_action, parse_args_with_context};
use crabgent_core::{
    Action, EmbeddingError, EmbeddingProvider, EmbeddingRequest, MAX_SEARCH_LIMIT, PolicyHook,
    RunCtx, RunId,
};
use crabgent_store::traits::MemoryStore;
use serde_json::{Value, json};

use crate::ops::{Args, Op};

const TOOL_NAME: &str = "memory";

const DESCRIPTION: &str = "Long-term memory storage. Operations: \
    `search` (full-text query, returns ranked hits), \
    `store` (persist a fact), \
    `get` (fetch by id), \
    `delete` (remove by id), \
    `relation_store` (link two documents with a typed edge), \
    `relation_delete` (remove an edge), \
    `relation_expand` (bounded breadth-first walk of edges from a root). \
    Store accepts optional `class`, `importance`, and `expires_at`; \
    search accepts optional `class`, `include_expired`, and `include_archived`. \
    Relation ops take `from_id`/`to_id` (memory ids) and a `relation_type` \
    label (`relation_store`/`relation_delete`); `relation_expand` takes a \
    `from_id` root and an optional `depth` (default 3, capped at 3) and \
    returns at most 128 nodes, setting `truncated: true` when that node cap \
    is hit. Every call requires a `scope` object with at least `owner` set; \
    additional fields (`channel`, `conv`, `agent`, `kind`) narrow the \
    scope further. The configured policy hook decides which scopes a \
    given subject may touch.";

/// LLM-facing memory tool. Holds the store + policy by `Arc`; clone is
/// cheap.
pub struct MemoryTool {
    pub(crate) store: Arc<dyn MemoryStore>,
    policy: Arc<dyn PolicyHook>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
}

impl MemoryTool {
    pub fn new(
        store: Arc<dyn MemoryStore>,
        policy: Arc<dyn PolicyHook>,
        embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    ) -> Self {
        Self {
            store,
            policy,
            embedding_provider,
        }
    }

    pub(crate) async fn gate(&self, action: &Action, ctx: &ToolCtx) -> Result<(), ToolError> {
        gate_tool_action(self.policy.as_ref(), ctx, action).await
    }

    pub(crate) async fn embed_text(
        &self,
        op: &'static str,
        text: &str,
        ctx: &ToolCtx,
    ) -> Result<Option<Vec<f32>>, ToolError> {
        let Some(provider) = self.embedding_provider.as_ref() else {
            return Ok(None);
        };
        if text.trim().is_empty() {
            return Ok(None);
        }
        if ctx.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        let run_ctx = embedding_run_ctx(ctx);
        let request = EmbeddingRequest {
            texts: vec![text.to_owned()],
            model: None,
        };
        match provider.embed(request, &run_ctx, ctx.cancel.as_ref()).await {
            Ok(response) => Ok(extract_single_vector(op, response.vectors, response.dim)),
            Err(EmbeddingError::Cancelled) => Err(ToolError::Cancelled),
            Err(err) => {
                crabgent_log::warn!(
                    op,
                    error_kind = embedding_error_kind(&err),
                    retry_after_secs = embedding_error_retry_after(&err),
                    "memory embedding failed; falling back to full-text search"
                );
                Ok(None)
            }
        }
    }
}

const fn embedding_error_kind(err: &EmbeddingError) -> &'static str {
    match err {
        EmbeddingError::Auth(_) => "auth",
        EmbeddingError::RateLimited { .. } => "rate_limited",
        EmbeddingError::Transport(_) => "transport",
        EmbeddingError::MalformedResponse(_) => "malformed_response",
        EmbeddingError::Cancelled => "cancelled",
        EmbeddingError::Timeout => "timeout",
        EmbeddingError::Other(_) => "other",
        _ => "unknown",
    }
}

const fn embedding_error_retry_after(err: &EmbeddingError) -> Option<u64> {
    match err {
        EmbeddingError::RateLimited { retry_after_secs } => *retry_after_secs,
        _ => None,
    }
}

fn embedding_run_ctx(ctx: &ToolCtx) -> RunCtx {
    let mut run_ctx = RunCtx::new(RunId::new(), ctx.subject.clone());
    if let Some(cancel) = &ctx.cancel {
        run_ctx = run_ctx.with_cancel(cancel.clone());
    }
    if let Some(session_id) = &ctx.session_id
        && run_ctx.set_session_id(session_id.clone()).is_err()
    {
        crabgent_log::warn!("memory embedding run context rejected session id");
    }
    run_ctx
}

fn extract_single_vector(op: &'static str, vectors: Vec<Vec<f32>>, dim: usize) -> Option<Vec<f32>> {
    let mut vectors = vectors.into_iter();
    let vector = first_vector(op, &mut vectors)?;
    if has_extra_vector(op, &mut vectors) {
        return None;
    }
    validate_vector_dim(op, &vector, dim)?;
    Some(vector)
}

fn first_vector(
    op: &'static str,
    vectors: &mut impl Iterator<Item = Vec<f32>>,
) -> Option<Vec<f32>> {
    let vector = vectors.next();
    if vector.is_none() {
        crabgent_log::warn!(op, "memory embedding provider returned no vectors");
    }
    vector
}

fn has_extra_vector(op: &'static str, vectors: &mut impl Iterator<Item = Vec<f32>>) -> bool {
    let has_extra = vectors.next().is_some();
    if has_extra {
        crabgent_log::warn!(
            op,
            "memory embedding provider returned more than one vector"
        );
    }
    has_extra
}

fn validate_vector_dim(op: &'static str, vector: &[f32], dim: usize) -> Option<()> {
    if vector.len() == dim {
        return Some(());
    }
    crabgent_log::warn!(
        op,
        actual_dim = vector.len(),
        expected_dim = dim,
        "memory embedding provider returned a vector with the wrong dimension"
    );
    None
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["op", "scope"],
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["search", "store", "get", "delete", "relation_store", "relation_delete", "relation_expand"],
                    "description": "Operation to perform."
                },
                "scope": crabgent_core::tool::memory_scope_schema(),
                "query": {"type": "string", "description": "Full-text query for op=search."},
                "body": {"type": "string", "description": "Document body for op=store."},
                "class": {"type": "string", "enum": ["semantic", "episodic", "notes", "user_profile", "skill", "tools"], "description": "Optional memory class for op=store/search. Pick the semantic category: `semantic` for general factual memories, `episodic` for time-bounded events, `notes` for general notes, `user_profile` for identity and preference facts about a user, `skill` for procedural knowledge, `tools` for tool-usage knowledge. The `kind` scope field is the channel-kind (direct/group/im) and is independent of `class`."},
                "importance": {"type": "number", "minimum": 0.0, "maximum": 1.0, "description": "Optional importance for op=store."},
                "expires_at": {"type": ["string", "null"], "format": "date-time", "description": "Optional expiry timestamp for op=store."},
                "include_expired": {"type": "boolean", "default": false, "description": "Include expired records for op=search."},
                "include_archived": {"type": "boolean", "default": false, "description": "Include archived records for op=search."},
                "doc_id": {"type": "string", "description": "Memory id for op=get/delete."},
                "from_id": {"type": "string", "description": "Source memory id for op=relation_store/relation_delete, or the root for op=relation_expand."},
                "to_id": {"type": "string", "description": "Target memory id for op=relation_store/relation_delete."},
                "relation_type": {"type": "string", "description": "Edge label for op=relation_store/relation_delete. Must start with a letter and contain only letters, digits, or underscores."},
                "depth": {"type": "integer", "minimum": 1, "maximum": 3, "description": "Max BFS hops for op=relation_expand (default 3, capped at 3). Expansion returns at most 128 nodes and flags `truncated` when the node cap is hit."},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT, "description": "Max hits for op=search (default 10, max 100)."},
                "offset": {"type": "integer", "minimum": 0, "description": "Pagination offset for op=search."},
                "since": {"type": "string", "format": "date-time"},
                "until": {"type": "string", "format": "date-time"}
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let parsed: Args = parse_args_with_context(args, "memory args")?;
        let scope = parsed.scope.clone();
        match parsed.op {
            Op::Search => crate::ops::search::do_search(self, &parsed, scope, ctx).await,
            Op::Store => crate::ops::store::do_store(self, &parsed, scope, ctx).await,
            Op::Get => crate::ops::get_delete::do_get(self, &parsed, scope, ctx).await,
            Op::Delete => crate::ops::get_delete::do_delete(self, &parsed, scope, ctx).await,
            Op::RelationStore => {
                crate::ops::relations::do_relation_store(self, &parsed, scope, ctx).await
            }
            Op::RelationDelete => {
                crate::ops::relations::do_relation_delete(self, &parsed, scope, ctx).await
            }
            Op::RelationExpand => {
                crate::ops::relations::do_relation_expand(self, &parsed, scope, ctx).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::policy::AllowAllPolicy;
    use crabgent_core::{MemoryScope, Owner, PolicyDecision, Subject, Tool};
    use std::sync::Mutex;

    use crate::ops::test_support::{alice_ctx, alice_scope_value, make_tool};

    struct OnlySearchPolicy;

    #[async_trait]
    impl PolicyHook for OnlySearchPolicy {
        async fn allow(&self, _: &Subject, action: &Action) -> PolicyDecision {
            if matches!(action, Action::MemorySearch { .. }) {
                PolicyDecision::Allow
            } else {
                PolicyDecision::Deny(format!("only search allowed, got {}", action.name()))
            }
        }
    }

    #[tokio::test]
    async fn typed_action_lets_policy_distinguish_ops() {
        let (tool, _) = make_tool(Arc::new(OnlySearchPolicy));
        let search_args = json!({
            "op": "search",
            "scope": alice_scope_value(),
            "query": "x"
        });
        tool.execute(search_args, &alice_ctx())
            .await
            .expect("test result");

        let store_args = json!({
            "op": "store",
            "scope": alice_scope_value(),
            "body": "x"
        });
        let err = tool
            .execute(store_args, &alice_ctx())
            .await
            .expect_err("expected error");
        assert!(matches!(err, ToolError::Permission(msg) if msg.contains("only search allowed")));
    }

    struct ScopeRecordingPolicy {
        seen: Mutex<Vec<MemoryScope>>,
    }

    #[async_trait]
    impl PolicyHook for ScopeRecordingPolicy {
        async fn allow(&self, _: &Subject, action: &Action) -> PolicyDecision {
            if let Some(scope) = action.scope() {
                self.seen
                    .lock()
                    .expect("mutex should not be poisoned")
                    .push(scope.clone());
            }
            PolicyDecision::Allow
        }
    }

    #[tokio::test]
    async fn scope_passes_to_policy_unmodified() {
        let policy = Arc::new(ScopeRecordingPolicy {
            seen: Mutex::new(Vec::new()),
        });
        let (tool, _) = make_tool(policy.clone());
        let args = json!({
            "op": "search",
            "scope": {
                "owner": "alice",
                "channel": "slack",
                "kind": "direct"
            },
            "query": "x"
        });
        tool.execute(args, &alice_ctx()).await.expect("test result");
        let seen = policy.seen.lock().expect("mutex should not be poisoned");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].owner, Some(Owner::new("alice")));
        assert_eq!(seen[0].channel.as_deref(), Some("slack"));
        assert_eq!(seen[0].kind.as_deref(), Some("direct"));
    }

    #[tokio::test]
    async fn schema_lists_op_enum_and_required_fields() {
        let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
        let schema = tool.parameters_schema();
        assert_eq!(schema["properties"]["op"]["enum"][0], "search");
        let required = schema["required"]
            .as_array()
            .expect("value should be an array");
        assert!(required.iter().any(|v| v == "op"));
        assert!(required.iter().any(|v| v == "scope"));
    }

    #[tokio::test]
    async fn schema_class_enum_accepts_all_six_values() {
        let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
        let schema = tool.parameters_schema();
        let class_enum = schema["properties"]["class"]["enum"]
            .as_array()
            .expect("class enum is an array");
        let names: Vec<&str> = class_enum.iter().filter_map(|v| v.as_str()).collect();
        for expected in [
            "semantic",
            "episodic",
            "notes",
            "user_profile",
            "skill",
            "tools",
        ] {
            assert!(
                names.contains(&expected),
                "schema class enum missing {expected}, got {names:?}"
            );
        }
        assert_eq!(names.len(), 6, "schema class enum has unexpected variants");
    }
}
