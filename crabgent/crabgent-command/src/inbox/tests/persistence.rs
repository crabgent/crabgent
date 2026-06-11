//! Persistence and reply-shape tests for `CommandDispatchInbox`.

use std::sync::{Arc, Mutex};

use crabgent_channel::ChannelInbox;
use crabgent_core::{ContentBlock, Message, Owner};
use crabgent_store::SessionStore;

use super::helpers::{OrderedPolicy, dispatch_scope, event, inbox};

#[tokio::test]
async fn dispatch_writes_save_messages_on_success() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(OrderedPolicy {
        order: Arc::clone(&order),
        deny_outer: false,
    });
    let (inbox, _inner, _sink, store) = inbox(policy, order);

    inbox
        .receive(event("/stub arg"))
        .await
        .expect("dispatch ok");

    let session = store
        .find_or_create(&Owner::new("test:conv"), None, &dispatch_scope())
        .await
        .expect("load session");
    assert_eq!(session.scope.agent.as_deref(), Some("worker"));
    assert_eq!(session.messages.len(), 2);
    assert!(
        matches!(&session.messages[0], Message::User { content, ..} if matches!(&content[0], ContentBlock::Text { text } if text == "/stub arg"))
    );
    assert!(matches!(&session.messages[1], Message::Assistant { text, .. } if text == "stub ok"));
}

#[tokio::test]
async fn command_session_reuses_agent_session_when_one_exists() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(OrderedPolicy {
        order: Arc::clone(&order),
        deny_outer: false,
    });
    let (inbox, _inner, _sink, store) = inbox(policy, order);
    let seeded = store
        .find_or_create(&Owner::new("test:conv"), None, &dispatch_scope())
        .await
        .expect("seed agent-scoped session");

    inbox
        .receive(event("/stub arg"))
        .await
        .expect("dispatch ok");

    let loaded = store
        .load(&seeded.id)
        .await
        .expect("load seeded session")
        .expect("seeded session exists");
    assert_eq!(loaded.messages.len(), 2);
    let rows = store
        .list(&Owner::new("test:conv"), crabgent_store::Page::first(10))
        .await
        .expect("list sessions");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, seeded.id);
}

#[tokio::test]
async fn dispatch_replies_are_not_threaded() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(OrderedPolicy {
        order: Arc::clone(&order),
        deny_outer: false,
    });
    let (inbox, _inner, sink, _store) = inbox(policy, order);

    inbox
        .receive(event("/stub arg"))
        .await
        .expect("dispatch ok");

    let parents = sink
        .thread_parents
        .lock()
        .expect("test mutex must not be poisoned");
    assert!(
        parents.iter().all(Option::is_none),
        "command replies must travel as top-level conv messages, got thread parents {parents:?}",
    );
}

#[tokio::test]
async fn dispatch_error_replies_are_not_threaded() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(OrderedPolicy {
        order: Arc::clone(&order),
        deny_outer: true,
    });
    let (inbox, _inner, sink, _store) = inbox(policy, order);

    inbox
        .receive(event("/stub arg"))
        .await
        .expect("deny handled");

    let parents = sink
        .thread_parents
        .lock()
        .expect("test mutex must not be poisoned");
    assert!(
        parents.iter().all(Option::is_none),
        "error replies must travel as top-level conv messages, got thread parents {parents:?}",
    );
}
