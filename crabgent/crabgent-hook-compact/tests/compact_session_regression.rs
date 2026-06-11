use std::sync::Arc;

use crabgent_core::MemoryScope;
use crabgent_core::{ContentBlock, Decision, Hook, Message, Owner, RunCtx, RunId, Subject};
use crabgent_hook_compact::CompactHook;
use crabgent_store::memory::MemorySessionStore;
use crabgent_store::{SessionId, SessionStore};
use crabgent_test_support::{StubProvider, user_msg as user};

const FIXED_SUMMARY: &str = "FIXED_SUMMARY";

fn message_text(message: &Message) -> String {
    match message {
        Message::User { content, .. } => content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Message::Assistant { text, .. } | Message::System { content: text } => text.clone(),
        Message::ToolResult { output, .. } => output.to_string(),
        other => format!("{other:?}"),
    }
}

async fn stored_session(store: &Arc<MemorySessionStore>, messages: Vec<Message>) -> SessionId {
    let mut session = store
        .find_or_create(&Owner::new("u"), None, &MemoryScope::default())
        .await
        .expect("session created");
    session.messages = messages;
    store.save(&session).await.expect("session saved");
    session.id
}

fn ctx_for_session(session_id: &SessionId) -> RunCtx {
    let ctx = RunCtx::new(RunId::new(), Subject::new("u"));
    ctx.set_session_id(session_id.to_string())
        .expect("session id is unset");
    ctx
}

fn replace_messages(decision: Decision<Vec<Message>>) -> Vec<Message> {
    match decision {
        Decision::Replace(messages) => messages,
        other => {
            assert!(
                matches!(other, Decision::Replace(_)),
                "pre_compact should replace, got {other:?}"
            );
            Vec::new()
        }
    }
}

#[tokio::test]
async fn compact_session_and_direct_pre_compact_produce_equivalent_messages() {
    let original_messages = vec![user("old-1"), user("old-2"), user("latest")];
    let store = Arc::new(MemorySessionStore::default());
    let session_id = stored_session(&store, original_messages.clone()).await;
    let provider = Arc::new(StubProvider::with_text(FIXED_SUMMARY));
    let hook = CompactHook::new(provider, "summary-model")
        .with_max_messages(1)
        .with_keep_recent_messages(1)
        .with_session_store(Arc::clone(&store));
    let store_dyn: Arc<dyn SessionStore> = store.clone();

    hook.compact_session(store_dyn, session_id.clone(), Subject::new("u"))
        .await
        .expect("session compaction succeeds");
    let path_a_messages = store
        .load(&session_id)
        .await
        .expect("session load succeeds")
        .expect("session exists")
        .messages;

    let ctx_b = ctx_for_session(&session_id);
    let path_b_messages = replace_messages(hook.pre_compact(&original_messages, &ctx_b).await);

    // Two independent runs verify that the manual compact-session entrypoint
    // produces semantically equivalent messages to the auto-loop pre_compact path.
    assert_eq!(path_a_messages.len(), path_b_messages.len());
    for (path_a, path_b) in path_a_messages.iter().zip(path_b_messages.iter()) {
        assert_eq!(message_text(path_a), message_text(path_b));
    }
}
