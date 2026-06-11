use std::sync::Arc;

use crabgent_core::hook::Hook;
use crabgent_core::{ContentBlock, Message, ReasoningEffort, RunCtx, RunId, Subject};
use crabgent_session::SessionPersistHook;
use crabgent_store::memory::MemorySessionStore;
use crabgent_store::{Owner, SessionStore};

#[tokio::test]
async fn flush_messages_does_not_clobber_model_override_written_mid_run() {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));

    let owner = Owner::new("clobber-user");
    let session_id = store
        .find_or_create(&owner, None, &crabgent_core::MemoryScope::default())
        .await
        .expect("seed find_or_create")
        .id;

    let ctx = RunCtx::new(RunId::new(), Subject::new("clobber-user"));
    hook.on_session_start(&ctx).await;

    let mut tool_view = store
        .load(&session_id)
        .await
        .expect("load")
        .expect("session exists");
    tool_view.model_override = Some("claude-opus-4-6".into());
    tool_view.reasoning_effort_override = Some(ReasoningEffort::High);
    store.save(&tool_view).await.expect("tool save");

    let msgs = vec![Message::User {
        content: vec![ContentBlock::Text {
            text: "post-tool".into(),
        }],
        timestamp: None,
    }];
    hook.on_message(&msgs, &ctx).await;

    let after = store
        .load(&session_id)
        .await
        .expect("load")
        .expect("session exists");
    assert_eq!(
        after.model_override.as_deref(),
        Some("claude-opus-4-6"),
        "SessionPersistHook::flush_messages must not clobber model_override \
         written to the store mid-run"
    );
    assert_eq!(
        after.reasoning_effort_override,
        Some(ReasoningEffort::High),
        "SessionPersistHook::flush_messages must not clobber \
         reasoning_effort_override written to the store mid-run"
    );
    assert_eq!(
        after.messages.len(),
        1,
        "messages should have been persisted by flush_messages"
    );
}
