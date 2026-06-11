//! Pins the `Outcome::Paused` persistence contract of
//! [`SessionPersistHook`]: completed turns (including assistant text and
//! resolved tool exchanges) survive in the session because they ARE the
//! resume state; only a trailing unresolved tool exchange is trimmed.
//! Cancelled runs keep the conservative safe-prefix policy.

use std::sync::Arc;

use crabgent_core::hook::{Hook, Outcome};
use crabgent_core::types::ToolCall;
use crabgent_core::{ContentBlock, Message, RunCtx, RunId, Subject};
use crabgent_session::SessionPersistHook;
use crabgent_store::memory::MemorySessionStore;
use crabgent_store::{Owner, SessionStore};
use serde_json::json;

fn user(text: &str) -> Message {
    Message::User {
        content: vec![ContentBlock::Text { text: text.into() }],
        timestamp: None,
    }
}

fn assistant_with_call(id: &str) -> Message {
    Message::Assistant {
        text: "working".into(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: "bash".into(),
            args: json!({}),
            thought_signature: None,
        }],
    }
}

fn tool_result(call_id: &str) -> Message {
    Message::ToolResult {
        call_id: call_id.into(),
        output: json!("ok"),
        is_error: false,
    }
}

async fn run_to_stop(outcome: Outcome, log: Vec<Message>, owner: &str) -> Vec<Message> {
    let store = Arc::new(MemorySessionStore::default());
    let hook = SessionPersistHook::new(Arc::clone(&store));
    let session_id = store
        .find_or_create(
            &Owner::new(owner),
            None,
            &crabgent_core::MemoryScope::default(),
        )
        .await
        .expect("seed find_or_create")
        .id;
    let ctx = RunCtx::new(RunId::new(), Subject::new(owner));
    hook.on_session_start(&ctx).await;
    hook.on_message(&log, &ctx).await;
    hook.on_stop(&ctx, &outcome).await;
    store
        .load(&session_id)
        .await
        .expect("load")
        .expect("session exists")
        .messages
}

#[tokio::test]
async fn paused_run_keeps_completed_turns_in_session() {
    let log = vec![user("input"), assistant_with_call("c1"), tool_result("c1")];
    let saved = run_to_stop(Outcome::Paused, log, "paused-user").await;
    assert_eq!(
        saved.len(),
        3,
        "completed assistant/tool turns are the resume state and must survive"
    );
}

#[tokio::test]
async fn paused_run_repairs_the_dangling_tool_exchange() {
    let log = vec![
        user("input"),
        assistant_with_call("c1"),
        tool_result("c1"),
        assistant_with_call("c2"),
    ];
    let saved = run_to_stop(Outcome::Paused, log, "paused-dangling-user").await;
    assert_eq!(
        saved.len(),
        5,
        "resolved c1 survives; dangling c2 gets a synthetic interrupted result"
    );
    match saved.last() {
        Some(Message::ToolResult {
            call_id, is_error, ..
        }) => {
            assert_eq!(call_id, "c2");
            assert!(is_error);
        }
        other => panic!("expected synthetic tool result, got {other:?}"),
    }
}

#[tokio::test]
async fn cancelled_run_keeps_the_conservative_safe_prefix() {
    let log = vec![user("input"), assistant_with_call("c1"), tool_result("c1")];
    let saved = run_to_stop(Outcome::Cancelled, log, "cancelled-user").await;
    assert_eq!(
        saved.len(),
        1,
        "cancelled runs still persist only the user/input prefix, by design"
    );
    assert!(matches!(saved.first(), Some(Message::User { .. })));
}
