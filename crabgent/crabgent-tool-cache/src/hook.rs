//! [`ToolCacheHook`]: compacts oversized `after_tool` outputs into the
//! [`ToolCacheStore`] and replaces the inline result with a compact
//! [`TruncatedOutput`](crate::TruncatedOutput) object.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use crabgent_core::hook::{Decision, Hook, RunCtx};
use crabgent_core::types::{ToolCall, ToolResult};
use crabgent_log::warn;
use crabgent_store::{ToolCacheEntry, ToolCacheStore};
use serde_json::Value;
use uuid::Uuid;

use crate::config::{CACHE_READ_TOOL_NAME, DEFAULT_PREVIEW_BYTES, ToolCacheConfig, default_ttl};
use crate::preview::smart_preview;
use crate::resolver::{SessionResolver, default_session_resolver};
use crate::truncated::TruncatedOutput;
use crabgent_core::tokens::estimate_tokens;

/// Hook that compacts oversized tool outputs into a [`ToolCacheStore`].
///
/// On `after_tool`, the hook checks if the tool result's textual output
/// exceeds `min_tokens`. If so, it persists the full content as a
/// [`ToolCacheEntry`] keyed by a fresh `cache_id` and the resolved
/// [`crabgent_store::SessionId`], then replaces the result content with a compact
/// [`TruncatedOutput`] plus instructions to call `cache_read` or spawn a task.
///
/// The hook is fail-soft: if the store insert fails, the original
/// result is forwarded unchanged and a `warn!` is logged. The LLM still
/// sees the (large) output rather than losing data.
///
/// ## Customisation
///
/// - [`Self::with_ttl`]: how long entries live before
///   [`ToolCacheStore::cleanup_expired`] removes them. Default 24h.
/// - [`Self::with_min_tokens`]: token threshold above which output is
///   cached. Default 4096 tokens.
/// - [`Self::with_preview_bytes`]: how much of the output is kept as
///   inline preview before "...". Default 256 bytes.
/// - [`Self::with_shared_session_resolver`]: installs the same resolver
///   as [`crate::CacheReadTool`], so hook and tool can read the same entry.
pub struct ToolCacheHook<C: ToolCacheStore> {
    store: Arc<C>,
    resolve_session: SessionResolver,
    ttl: Duration,
    config: ToolCacheConfig,
    preview_bytes: usize,
}

impl<C: ToolCacheStore> ToolCacheHook<C> {
    pub fn new(store: Arc<C>) -> Self {
        Self {
            store,
            resolve_session: default_session_resolver(),
            ttl: default_ttl(),
            config: ToolCacheConfig::default(),
            preview_bytes: DEFAULT_PREVIEW_BYTES,
        }
    }

    #[must_use]
    pub const fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    #[must_use]
    pub const fn with_min_tokens(mut self, tokens: usize) -> Self {
        self.config.min_tokens = tokens;
        self
    }

    #[must_use]
    pub fn with_config(mut self, config: ToolCacheConfig) -> Self {
        self.config = config;
        self
    }

    #[must_use]
    pub const fn with_preview_bytes(mut self, bytes: usize) -> Self {
        self.preview_bytes = bytes;
        self
    }

    #[must_use]
    pub fn with_shared_session_resolver(mut self, resolver: SessionResolver) -> Self {
        self.resolve_session = resolver;
        self
    }
}

#[async_trait]
impl<C> Hook for ToolCacheHook<C>
where
    C: ToolCacheStore + 'static,
{
    async fn after_tool(
        &self,
        call: &ToolCall,
        result: &ToolResult,
        ctx: &RunCtx,
    ) -> Decision<ToolResult> {
        if result.is_error || call.name == CACHE_READ_TOOL_NAME {
            return Decision::Continue;
        }
        let threshold = self
            .config
            .tool_overrides
            .get(&call.name)
            .copied()
            .unwrap_or(self.config.min_tokens);
        let content = match output_content(&result.output) {
            Ok(content) => content,
            Err(e) => {
                warn!(
                    tool = %call.name,
                    error = %e,
                    "tool-cache hook: output serialization failed; leaving inline output"
                );
                return Decision::Continue;
            }
        };
        let size_tokens = estimate_tokens(&content);
        if !should_cache(size_tokens, threshold) {
            return Decision::Continue;
        }
        let session_id = (self.resolve_session)(&ctx.subject);
        let cache_id = Uuid::now_v7().to_string();
        let preview = smart_preview(&content, self.preview_bytes);
        let now = Utc::now();
        let entry = ToolCacheEntry {
            id: cache_id.clone(),
            session_id,
            tool_name: call.name.clone(),
            content: content.clone(),
            preview: preview.clone(),
            created_at: now,
            expires_at: now + self.ttl,
        };
        if let Err(e) = self.store.insert(&entry).await {
            warn!(
                cache_id = %cache_id,
                tool = %call.name,
                error_kind = e.kind(),
                transient = e.is_transient(),
                "tool-cache hook: insert failed; leaving inline output"
            );
            return Decision::Continue;
        }
        let truncated = TruncatedOutput::new(cache_id.clone(), size_tokens, preview);
        let replacement = match serde_json::to_value(truncated) {
            Ok(value) => value,
            Err(e) => {
                warn!(
                    cache_id = %cache_id,
                    tool = %call.name,
                    error = %e,
                    "tool-cache hook: replacement serialization failed; leaving inline output"
                );
                return Decision::Continue;
            }
        };
        let mut replaced = result.clone();
        replaced.output = replacement;
        replaced.is_error = false;
        Decision::Replace(replaced)
    }
}

const fn should_cache(size_tokens: usize, min_tokens: usize) -> bool {
    size_tokens >= min_tokens
}

fn output_content(output: &Value) -> serde_json::Result<String> {
    match output {
        Value::String(s) => Ok(s.clone()),
        other => serde_json::to_string(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crabgent_core::Subject;
    use crabgent_core::run_id::RunId;
    use crabgent_store::SessionId;
    use crabgent_store::memory::MemoryToolCacheStore;
    use serde_json::json;

    use crate::resolver::default_session_id;
    use crate::truncated::TruncatedOutput;

    fn run_ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("user-1"))
    }

    fn ok_result(call_id: &str, output: Value) -> ToolResult {
        ToolResult::success(output).with_call_id(call_id)
    }

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            args: json!({}),
            thought_signature: None,
        }
    }

    #[tokio::test]
    async fn small_output_passes_through() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let hook = ToolCacheHook::new(Arc::clone(&store)).with_min_tokens(1024);
        let ctx = run_ctx();
        let dec = hook
            .after_tool(
                &call("c1", "bash"),
                &ok_result("c1", Value::String("hi".into())),
                &ctx,
            )
            .await;
        assert!(matches!(dec, Decision::Continue));
    }

    #[tokio::test]
    async fn oversize_output_is_cached_and_replaced() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let hook = ToolCacheHook::new(Arc::clone(&store))
            .with_min_tokens(1)
            .with_preview_bytes(4);
        let ctx = run_ctx();
        let big = "abcdefghijklmnop".to_owned();
        let dec = hook
            .after_tool(
                &call("c2", "bash"),
                &ok_result("c2", Value::String(big)),
                &ctx,
            )
            .await;
        let Decision::Replace(replaced) = dec else {
            panic!("expected Replace, got {dec:?}");
        };
        let truncated: TruncatedOutput =
            serde_json::from_value(replaced.output.clone()).expect("test result");
        assert!(truncated.cached);
        assert_eq!(truncated.preview, "abcd...");
        assert!(truncated.hint.contains("cache_read"));
        assert!(truncated.hint.contains("task"));
        assert!(!replaced.is_error);
        assert_eq!(replaced.call_id, "c2");
    }

    #[tokio::test]
    async fn error_results_skip_cache() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let hook = ToolCacheHook::new(Arc::clone(&store)).with_min_tokens(1);
        let ctx = run_ctx();
        let err_result = ToolResult::soft_error(Value::String("x".repeat(64))).with_call_id("c3");
        let dec = hook
            .after_tool(&call("c3", "bash"), &err_result, &ctx)
            .await;
        assert!(matches!(dec, Decision::Continue));
    }

    #[tokio::test]
    async fn cache_read_tool_skips_cache() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let hook = ToolCacheHook::new(Arc::clone(&store)).with_min_tokens(1);
        let ctx = run_ctx();
        let res = ok_result("c4", Value::String("x".repeat(64)));
        let dec = hook.after_tool(&call("c4", "cache_read"), &res, &ctx).await;
        assert!(matches!(dec, Decision::Continue));
    }

    #[tokio::test]
    async fn large_object_output_is_cached() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let hook = ToolCacheHook::new(Arc::clone(&store)).with_min_tokens(1);
        let ctx = run_ctx();
        let output = json!({"data": "x".repeat(64)});
        let expected_content = serde_json::to_string(&output).expect("test result");
        let res = ok_result("c5", output);
        let dec = hook.after_tool(&call("c5", "bash"), &res, &ctx).await;
        let Decision::Replace(replaced) = dec else {
            panic!("expected Replace");
        };
        let truncated: TruncatedOutput =
            serde_json::from_value(replaced.output).expect("test result");
        let session_id = default_session_id(&ctx.subject);
        let entry = store
            .get(&truncated.cache_id, &session_id)
            .await
            .expect("test result")
            .expect("test result");
        assert_eq!(entry.content, expected_content);
    }

    #[tokio::test]
    async fn small_object_output_skips_cache() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let hook = ToolCacheHook::new(Arc::clone(&store)).with_min_tokens(1024);
        let ctx = run_ctx();
        let res = ok_result("c5", json!({"data": "small"}));
        let dec = hook.after_tool(&call("c5", "bash"), &res, &ctx).await;
        assert!(matches!(dec, Decision::Continue));
    }

    #[tokio::test]
    async fn cached_entry_lookup_recovers_full_content() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let hook = ToolCacheHook::new(Arc::clone(&store))
            .with_min_tokens(1)
            .with_preview_bytes(4);
        let ctx = run_ctx();
        let big = "1234567890abcdef".to_owned();
        let dec = hook
            .after_tool(
                &call("c6", "bash"),
                &ok_result("c6", Value::String(big.clone())),
                &ctx,
            )
            .await;
        let Decision::Replace(replaced) = dec else {
            panic!("expected Replace");
        };
        let truncated: TruncatedOutput =
            serde_json::from_value(replaced.output).expect("test result");
        let session_id = default_session_id(&ctx.subject);
        let entry = store
            .get(&truncated.cache_id, &session_id)
            .await
            .expect("test result")
            .expect("test result");
        assert_eq!(entry.content, big);
    }

    #[tokio::test]
    async fn custom_session_resolver_overrides_default() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let custom = SessionId::new();
        let hook = ToolCacheHook::new(Arc::clone(&store))
            .with_min_tokens(1)
            .with_preview_bytes(1)
            .with_shared_session_resolver(Arc::new({
                let s = custom.clone();
                move |_: &Subject| s.clone()
            }));
        let ctx = run_ctx();
        let dec = hook
            .after_tool(
                &call("c7", "bash"),
                &ok_result("c7", Value::String("xxxxxx".into())),
                &ctx,
            )
            .await;
        let Decision::Replace(replaced) = dec else {
            panic!("expected Replace");
        };
        let truncated: TruncatedOutput =
            serde_json::from_value(replaced.output).expect("test result");
        assert!(
            store
                .get(&truncated.cache_id, &custom)
                .await
                .expect("test result")
                .is_some()
        );
    }

    #[tokio::test]
    async fn custom_ttl_propagates_to_entry() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let hook = ToolCacheHook::new(Arc::clone(&store))
            .with_min_tokens(1)
            .with_preview_bytes(1)
            .with_ttl(Duration::seconds(10));
        let ctx = run_ctx();
        let before = Utc::now();
        let dec = hook
            .after_tool(
                &call("c8", "bash"),
                &ok_result("c8", Value::String("xxxxxx".into())),
                &ctx,
            )
            .await;
        let Decision::Replace(replaced) = dec else {
            panic!("expected Replace");
        };
        let truncated: TruncatedOutput =
            serde_json::from_value(replaced.output).expect("test result");
        let session_id = default_session_id(&ctx.subject);
        let entry = store
            .get(&truncated.cache_id, &session_id)
            .await
            .expect("test result")
            .expect("test result");
        let after_window = before + Duration::seconds(60);
        assert!(entry.expires_at <= after_window);
    }

    #[test]
    fn token_threshold_default_4096() {
        let store = Arc::new(MemoryToolCacheStore::default());
        let hook = ToolCacheHook::new(store);

        assert_eq!(hook.config.min_tokens, crate::DEFAULT_MIN_TOKENS);
        assert_eq!(hook.config.min_tokens, 4096);
    }
}
