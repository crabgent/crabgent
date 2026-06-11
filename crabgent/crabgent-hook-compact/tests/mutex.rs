use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::{
    ContentBlock, Decision, Hook, LlmRequest, LlmResponse, Message, ModelInfo, Outcome, Provider,
    ProviderCapabilities, ProviderError, RunCtx, RunId, StopReason, Subject, Usage,
};
use crabgent_hook_compact::CompactHook;
use tokio_util::sync::CancellationToken;

struct YieldingProvider {
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl YieldingProvider {
    fn new() -> (Arc<Self>, Arc<Mutex<Vec<LlmRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Arc::new(Self {
                requests: Arc::clone(&requests),
            }),
            requests,
        )
    }
}

#[async_trait]
impl Provider for YieldingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(req.clone());
        tokio::task::yield_now().await;
        Ok(LlmResponse {
            text: "new summary".into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "yielding-summary"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("summary-model", "summary")]
    }
}

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("u"))
}

fn user(text: &str) -> Message {
    Message::User {
        content: vec![ContentBlock::Text { text: text.into() }],
        timestamp: None,
    }
}

#[tokio::test]
async fn parallel_pre_compact_serialized() {
    let (provider, requests) = YieldingProvider::new();
    let hook = CompactHook::new(provider, "summary-model")
        .with_max_messages(1)
        .with_keep_recent_messages(1);
    let run_ctx = ctx();
    let messages = [user("old request"), user("latest request")];

    let (first, second) = tokio::join!(
        hook.pre_compact(&messages, &run_ctx),
        hook.pre_compact(&messages, &run_ctx)
    );

    assert!(matches!(first, Decision::Replace(_)));
    assert!(matches!(second, Decision::Replace(_)));
    assert_eq!(requests.lock().expect("requests lock").len(), 1);

    let changed_messages = [user("changed old request"), user("changed latest request")];
    let changed = hook.pre_compact(&changed_messages, &run_ctx).await;

    assert!(matches!(changed, Decision::Replace(_)));
    assert_eq!(requests.lock().expect("requests lock").len(), 2);

    hook.on_stop(&run_ctx, &Outcome::Completed("ok".into()))
        .await;
    let after_stop = hook.pre_compact(&messages, &run_ctx).await;

    assert!(matches!(after_stop, Decision::Replace(_)));
    assert_eq!(requests.lock().expect("requests lock").len(), 3);
}
