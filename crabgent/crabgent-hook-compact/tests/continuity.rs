use std::sync::Arc;

use crabgent_core::MemoryScope;
use crabgent_core::{ContentBlock, Decision, Hook, LlmRequest, Message, RunCtx, RunId, Subject};
use crabgent_hook_compact::CompactHook;
use crabgent_store::memory::MemorySessionStore;
use crabgent_store::{Owner, SessionId, SessionStore};
use crabgent_test_support::{StubProvider, user_msg as user};

fn ctx_with_session(id: &SessionId) -> RunCtx {
    let ctx = RunCtx::new(RunId::new(), Subject::new("u"));
    ctx.set_session_id(id.to_string())
        .expect("session id should be set once");
    ctx
}

fn request_text(req: &LlmRequest) -> Option<String> {
    let message: Message = serde_json::from_value(req.messages.first()?.clone()).ok()?;
    let Message::User { content, .. } = message else {
        return None;
    };
    content.first().and_then(|block| match block {
        ContentBlock::Text { text } => Some(text.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn prior_summary_loaded_and_prepended() {
    let store = Arc::new(MemorySessionStore::default());
    let session = store
        .find_or_create(&Owner::new("u"), None, &MemoryScope::default())
        .await
        .expect("create session");
    store
        .set_compaction_summary(&session.id, "prior X")
        .await
        .expect("set prior summary");
    let ctx = ctx_with_session(&session.id);
    let provider = Arc::new(StubProvider::with_text("new summary"));
    let hook = CompactHook::new(Arc::clone(&provider), "summary-model")
        .with_session_store(Arc::clone(&store))
        .with_max_messages(1)
        .with_keep_recent_messages(1);

    let decision = hook
        .pre_compact(&[user("old request"), user("latest request")], &ctx)
        .await;

    assert!(matches!(decision, Decision::Replace(_)));
    let text = {
        let requests = provider.captured_requests();
        request_text(requests.first().expect("summary provider should be called"))
            .expect("summary request should contain text block")
    };
    assert!(text.contains("previously-compacted state"));
    assert!(text.contains("<prior_summary>\nprior X\n</prior_summary>"));
    assert!(text.contains("old request"));

    let stored = store
        .get_compaction_summary(&session.id)
        .await
        .expect("get stored summary");
    assert_eq!(stored.as_deref(), Some("new summary"));
}

#[tokio::test]
async fn no_store_skips_prior_summary() {
    let session_id = SessionId::new();
    let ctx = ctx_with_session(&session_id);
    let provider = Arc::new(StubProvider::with_text("new summary"));
    let hook = CompactHook::new(Arc::clone(&provider), "summary-model")
        .with_max_messages(1)
        .with_keep_recent_messages(1);

    let decision = hook
        .pre_compact(&[user("old request"), user("latest request")], &ctx)
        .await;

    assert!(matches!(decision, Decision::Replace(_)));
    let requests = provider.captured_requests();
    let text = request_text(requests.first().expect("summary provider should be called"))
        .expect("summary request should contain text block");
    assert!(!text.contains("<prior_summary>"));
    assert!(!text.contains("previously-compacted state"));
}
